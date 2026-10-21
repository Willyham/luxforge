// Sliced graph execution for ggml's Metal context, the local change THIRD_PARTY.md records. Not a
// translation unit of its own: native/build.rs inserts this text into its staged copy of the
// vendored ggml-metal-context.m, at file scope just before ggml_metal_graph_compute, which hands
// a graph to luxforge_graph_compute_sliced while a slice target is set.
#include <stdatomic.h>

// Luxforge: sliced graph execution. The GPU time a slice aims at, in microseconds; 0 runs graphs
// as upstream does. Process-wide, as the shim sets it around each generation.
static _Atomic(int64_t) luxforge_slice_us = 0;
// The non-empty nodes the next slice encodes, adapted after each slice towards the target.
static _Atomic(int) luxforge_slice_nodes = 8;

#define LUXFORGE_SLICE_NODES_MIN 1
#define LUXFORGE_SLICE_NODES_MAX 4096

void luxforge_ggml_metal_set_slice_us(int64_t slice_us) {
    atomic_store(&luxforge_slice_us, slice_us > 0 ? slice_us : 0);
}

// The graph index of the non-empty node `skip` non-empty nodes on from `start`, counted as
// ggml_metal_op counts them; the node count past the last. Called only with fewer than the
// non-empty nodes before the encoder's end, so the node found lies before it.
static int luxforge_node_after(const struct ggml_cgraph * gf, int start, int skip) {
    for (int i = start; i < gf->n_nodes; ++i) {
        if (ggml_op_is_empty(gf->nodes[i]->op) || ggml_is_empty(gf->nodes[i])) {
            continue;
        }
        if (skip == 0) {
            return i;
        }
        skip--;
    }
    return gf->n_nodes;
}

// Where upstream's command buffer holding node `start` ends: ggml_metal_graph_compute gives the
// first n_nodes_0 nodes to one buffer and splits the rest evenly between n_cb more. Fusion never
// crosses these points upstream, so a slice's encoder ends at the next one too.
//
// This mirrors upstream's split in ggml_metal_graph_compute (n_main, n_nodes_0, n_nodes_1 and
// n_nodes_per_cb, and each command buffer's range in the encode_async block ggml_metal_set_n_cb
// builds) as the pinned ggml (vendor/ggml-d25b121c) computes it; the sliced path never runs
// upstream's computation, so there is nothing to assert it against. Check it against upstream's
// again whenever ggml is updated: a split that no longer matches lets a slice fuse across a
// point upstream never does.
static int luxforge_split_after(ggml_metal_t ctx, const struct ggml_cgraph * gf, int start) {
    const int n_nodes = gf->n_nodes;
    if (ctx->n_cb == 0) {
        return n_nodes;
    }
    const int n_main    = MAX(64, 0.1*gf->n_nodes);
    const int n_nodes_0 = MIN(n_main, n_nodes);
    if (start < n_nodes_0) {
        return n_nodes_0;
    }
    const int n_nodes_1      = n_nodes - n_nodes_0;
    const int n_nodes_per_cb = (n_nodes_1 + ctx->n_cb - 1) / ctx->n_cb;
    for (int cb_idx = 0; cb_idx < ctx->n_cb; ++cb_idx) {
        const int idx_end = n_nodes_0 + MIN((cb_idx == ctx->n_cb - 1) ? n_nodes_1 : (cb_idx + 1) * n_nodes_per_cb, n_nodes_1);
        if (start < idx_end) {
            return idx_end;
        }
    }
    return n_nodes;
}

// Encode into `cmd_buf` the graph's nodes from `start` on, at least `want` non-empty ones where
// the graph has them, stopping only between fused groups: the encoder spans the rest of
// upstream's command buffer, so fusion looks exactly as far ahead as it does upstream. Answers
// where the next slice starts, and in `encoded` the non-empty nodes encoded.
static int luxforge_encode_slice(ggml_metal_t ctx, id<MTLCommandBuffer> cmd_buf, struct ggml_cgraph * gf, int start, int want, int * encoded) {
    const int end = luxforge_split_after(ctx, gf, start);
    ggml_metal_op_t ctx_op = ggml_metal_op_init(
        ctx->dev,
        cmd_buf,
        gf,
        ctx->finfo,
        start,
        end,
        ctx->use_concurrency,
        ctx->capture_compute,
        ctx->debug_graph);

    const int n = ggml_metal_op_n_nodes(ctx_op);
    int idx = 0;
    while (idx < n && idx < want) {
        const int res = ggml_metal_op_encode(ctx_op, idx);
        if (res == 0) {
            idx = n;
            break;
        }
        idx += res;
    }

    ggml_metal_op_free(ctx_op);

    *encoded = idx;
    return idx >= n ? end : luxforge_node_after(gf, start, idx);
}

// Run `gf` slice by slice: while one slice runs, the next is encoded; it is committed once the
// running one has completed, so this backend holds at most one command buffer on the GPU and
// other queues' work runs between slices. The last slice is kept as the last command buffer, as
// upstream keeps its last, for ggml_metal_synchronize.
//
// Each iteration drains its own autorelease pool, so a slice's encoders and command buffer go
// once it has completed rather than when the graph ends. A slice's command buffer outlives the
// iteration that encodes it, so it is retained as it is created, as upstream retains its own, and
// released once it has completed, unless it is the last, whose reference passes to cmd_bufs.
static enum ggml_status luxforge_graph_compute_sliced(ggml_metal_t ctx, struct ggml_cgraph * gf, int64_t slice_us) {
    if (ctx->capture_compute >= 0) {
        ctx->capture_compute--;
    }

    // keep the memory wired
    ggml_metal_device_rsets_keep_alive(ctx->dev);

    enum ggml_status status = GGML_STATUS_SUCCESS;

    @autoreleasepool {
        id<MTLCommandQueue> queue = ggml_metal_device_get_queue(ctx->dev);

        int start = 0;
        id<MTLCommandBuffer> running = nil;
        int running_nodes = 0;
        id<MTLCommandBuffer> last = nil;

        while (start < gf->n_nodes || running != nil) {
            @autoreleasepool {
                id<MTLCommandBuffer> next = nil;
                int next_start = start;
                int next_nodes = 0;
                if (start < gf->n_nodes) {
                    next = [queue commandBufferWithUnretainedReferences];
                    [next retain];
                    next_start = luxforge_encode_slice(ctx, next, gf, start, atomic_load(&luxforge_slice_nodes), &next_nodes);
                }

                if (running != nil) {
                    [running waitUntilCompleted];

                    if ([running status] != MTLCommandBufferStatusCompleted) {
                        GGML_LOG_ERROR("%s: error: a slice's command buffer failed with status %d\n", __func__, (int) [running status]);
                        if ([running status] == MTLCommandBufferStatusError) {
                            GGML_LOG_ERROR("error: %s\n", [[[running error] localizedDescription] UTF8String]);
                        }
                        ctx->has_error = true;
                        status = GGML_STATUS_FAILED;
                        [running release];
                        [next release];
                        break;
                    }

                    // Towards the node count that would have taken the target, halfway, so one
                    // long or short slice does not swing the next.
                    const double gpu_us = ([running GPUEndTime] - [running GPUStartTime]) * 1e6;
                    if (gpu_us > 0.0 && running_nodes > 0) {
                        const int want = atomic_load(&luxforge_slice_nodes);
                        double scaled = (double) running_nodes * (double) slice_us / gpu_us;
                        if (scaled > LUXFORGE_SLICE_NODES_MAX) {
                            scaled = LUXFORGE_SLICE_NODES_MAX;
                        }
                        int adapted = (int) (0.5 * ((double) want + scaled));
                        adapted = MAX(LUXFORGE_SLICE_NODES_MIN, MIN(LUXFORGE_SLICE_NODES_MAX, adapted));
                        atomic_store(&luxforge_slice_nodes, adapted);
                    }

                    if (ctx->abort_callback && ctx->abort_callback(ctx->abort_callback_data)) {
                        GGML_LOG_INFO("%s: sliced graph aborted\n", __func__);
                        status = GGML_STATUS_ABORTED;
                        [running release];
                        [next release];
                        break;
                    }

                    // The last slice, with no next, keeps its reference as `last`.
                    if (next != nil) {
                        [running release];
                    }
                }

                if (next != nil) {
                    [next commit];
                    last = next;
                }
                running = next;
                running_nodes = next_nodes;
                start = next_start;
            }
        }

        // `last` holds the reference retained as it was created.
        if (status == GGML_STATUS_SUCCESS && last != nil) {
            if (ctx->cmd_bufs[ctx->n_cb].obj) {
                [ctx->cmd_bufs[ctx->n_cb].obj release];
            }
            ctx->cmd_bufs[ctx->n_cb].obj = last;
            ctx->cmd_buf_last = last;
        }
    }

    return status;
}
