# Shared editing as a future extension

Status: future extension possibility. The owner supports exploring the host-led direction described here. This is a design plan, with no task breakdown, implementation authorization or scheduled milestone. Detailed interaction and protocol choices remain proposals. Current editor priorities continue to be RAW qualification, export, Locate, MCP and full-editor verification.

## Purpose and product value

Let a person invite another person or an agent to work alongside them on a photograph, using a desktop client, a possible browser client, or the shared programmatic interface. Everyone operates the same editing service and can understand which state they are seeing, who changed it and whether their work is committed.

The strongest candidate workflows are an agent helping with adjustments while the owner edits, remote teaching, and a photographer working with a retoucher. Shared viewing, demonstrating an adjustment and handing control to another participant may provide more value than two people continuously manipulating the same global slider. The first experiment should establish which interaction is useful before expanding the system.

The direction fits the pillars: originals remain read-only, every interaction has a semantic API, committed history retains attribution, and rendering stays bounded. Optional session transport and preview delivery should preserve the small core and ordinary local editing. An account service, cloud catalog and proprietary hosted dependency are not prerequisites.

## Foundation already present

The [architecture](architecture.md), [history contract](../specs/edit-history.md) and [module contract](modules-and-api.md) provide:

- One catalog owner and command service for the desktop and external JSON clients.
- Stable asset and layer identities, immutable complete recipe snapshots, attributed actions, monotonic revisions and request deduplication.
- Per-client view and preview state, with owner-held core drafts for adjustment gestures. The crop editor has its own desktop-local draft with equivalent explicit conflict handling.
- Field-patch actions for Basic adjustments; omitted fields are preserved.
- Validated recipe transactions and a renderer that consumes snapshots from verified originals.
- Bounded clients, events, requests and worker queues, including event-gap recovery by reading fresh state.

Local human–agent editing already uses these foundations. The live transport is same-user loopback access, not a remote invitation protocol. Drafts are private and emit no events; any asset revision change makes a draft conflicted. Undo and redo move the shared current pointer. Actor labels are supplied by callers, not authenticated remote identities. These distinctions are the principal gaps between the current service and a shared editing product.

## Proposed first experience

Keep the first evaluation to one host, one invited collaborator or agent, and one photograph. The host desktop remains the catalog owner and authoritative renderer. Participants submit semantic commands; the host validates and durably commits accepted changes, then distributes authoritative state and previews.

```text
Host desktop ───────┐
Invited client ─────┼── shared command service ── durable recipe snapshot
Agent/API client ──┘                                      │
                                                   bounded renderer
                                                         │
                                             identified client previews
```

An invited human could use a thin browser client or another desktop. Choosing the first client is an open product decision; the experiment does not require shipping both. A browser would display host-rendered previews and submit the same semantic operations, without first porting RAW decoding and development. It is a session client, separate from the excluded Lightroom-style Web publishing module.

Each participant retains independent zoom, pan, tool selection and historical preview. Following another participant's view could be an explicit, reversible mode. The interface should distinguish the committed shared image, a participant's in-progress gesture and a historical preview. A guest's presence or activity should be visible without requiring chat, audio or a general collaboration workspace.

The host's original and catalog stay local. Preview access does not imply original-file download. If a relay is needed for internet connectivity, it transports the session; it does not become the catalog owner. When the host is unavailable, guests cannot receive acknowledgements for new shared commits. Host handoff and independent offline editing are outside the first evaluation.

## Canonical state and the role of CRDTs

Retain the current recipe snapshot as the renderer's authoritative input. The host orders accepted commits and validates every resulting recipe through the existing service. It does not merge SQLite files or require the renderer to replay a distributed command log.

Materializing a canonical recipe is compatible with the proposed model. It must preserve committed history, attribution and source references rather than flattening edits into pixels or discarding competing intentions. Concurrent changes need explicit editing semantics even when their delivery order is known.

A CRDT can provide replica convergence under its merge rules, but it does not decide which photographic intention should win. A deterministic winner for two exposure assignments still leaves a meaningful user conflict. Likewise, a converged list of operations does not establish that crop, orientation and content-space dependencies remain valid.

The initial direction therefore requires no CRDT dependency. Reconsider one if a selected workflow needs independent offline edits, several writable replicas without a continuously available owner, or separately scoped replicated data. That would require a new design for causal identity, history, merge semantics, bounded synchronization metadata and preservation of unresolved work. Compaction could not silently discard information needed by returning replicas or by retained history.

## Conflict policy

Keep current revision checks as the safe baseline. A stale command must not be made to succeed merely by replacing its expected revision and retrying. A future automatic rebase should require a module-declared rule that proves the change remains valid, considering both the fields written and the state read to derive them.

| Concurrent activity | Proposed handling |
| --- | --- |
| Independent Basic exposure and saturation patches | Combine only when declared safe by the module, preserving each action and its attribution |
| Two assignments to exposure | Report contention; evaluate explicit resolution or temporary control of that adjustment |
| Crop and orientation changes | Treat dependent geometry as a semantic unit; revalidate or require explicit resolution |
| An agent or picker derives settings from a sampled image that changes | Validate the source/recipe state used for the calculation; separate output fields alone do not make the operation safe |
| A reset overlaps another edit | Treat the reset's complete affected parameter group as its write scope |
| An unknown module has no collaboration rule | Preserve strict revision conflict handling |

Core transaction and conflict machinery belongs in the shared service. Tool-specific dependency and rebase rules belong with the modules. The UI and transport must not hold alternate editing rules. Coupled parameter groups should remain atomic rather than being merged field by field into a state neither participant intended.

The first experiment can retain explicit Discard/Reapply and evaluate a visible handoff or temporary gesture ownership before introducing automatic merging. Any ownership mechanism needs expiry and disconnect behavior that cannot leave a control permanently unavailable. The exact policy remains open.

## Gestures, previews and presence

Separate durable actions from transient activity. Slider movement and cursor presence should use bounded, replaceable updates; a completed gesture should still create one history action. Cancellation must remove its transient preview without committing it.

Shared gesture previews need a new explicit channel because current drafts are private. Core adjustment drafts and the desktop crop draft need coherent observable identities and lifecycle semantics before both can be shared. Whether a participant sees only the active editor's draft or a validated composition of independent drafts is an open decision; composition must never bypass module validation.

Every delivered frame should identify its asset, source fingerprint, recipe/snapshot, relevant draft and render generation. An older frame must not replace a newer one or be labelled as committed when it represents a draft. Participant view state, presence and draft updates should not inflate persistent edit history.

Host rendering offers a common pixel source. Preview resolution, compression and network latency remain explicit quality limits, and different displays still need their own colour handling. If independent rendering on another desktop is later selected, matching original bytes, module implementations, processing parameters and profiles becomes part of the session contract. Equal recipe JSON alone is not proof of equal pixels. Unsupported providers or shapes must fail explicitly under the current-shapes policy.

## Undo, restore and alternative proposals

Collaborative undo is a product decision, not a transport detail. Today Undo navigates the shared current pointer. If one person changes exposure and another then crops, ordinary Undo returns past the crop regardless of who presses it.

The preferred direction to evaluate is "undo my last change": append a validated compensating action that preserves unrelated later edits, and report a conflict when subsequent work prevents a safe reversal. Redo needs the same dependency awareness. This would be an explicit addition to the shared command and history model; no client should implement it by silently restoring an older complete snapshot. Global history navigation and Restore need clearly distinguished shared-session behavior and permissions.

Agent-proposed alternatives are a separate extension. Existing [named versions](versions-and-lineage.md) point to retained entries; they are not independent editable branches. A workflow where an agent prepares an alternative for acceptance would need proposal/branch identity, acceptance semantics and provenance. The first shared session need not include it.

## Invitation, authority and recovery

An invitation should grant access to a selected asset and permitted operations, with authenticated participant identity and revocation. Bind durable attribution to that identity while retaining useful human/agent labels. Read-only viewing and editing are useful candidate roles; the final permission vocabulary is open.

The existing local token and caller-supplied actor are not sufficient as a remote authority model. Remote clients should not inherit unrestricted host filesystem access through import, Locate, export or diagnostics. Session authentication, transport protection, asset scoping and operation authorization need a concrete design before remote access is enabled. Agents use the same scoped service as humans.

Commit acknowledgements follow durable persistence. Reconnect should read fresh authoritative state, identify already accepted requests through deduplication, and reconcile pending intentions without duplicating or blindly replaying edits. Request identity must distinguish participants and survive the retries the session promises to support. Bounded event gaps require snapshot resynchronization.

Drafts currently disappear with their sessions. Retaining uncommitted guest work across a disconnect would be a new capability with explicit retention and recovery rules, not an assumed guarantee. The UI must distinguish pending, committed, conflicted and disconnected work. Ending a shared session must leave the host's committed photograph and history usable locally.

## Bounded implementation shape

Keep invitations, connection management and preview transport optional and inactive outside a shared session. Extend the core only for shared invariants such as authenticated command context, conflict semantics and collaborative undo. Continue using the same command registry for human and agent operations; proposed collaboration operations must be discoverable when implemented, but this plan defines no new command names.

Apply the [performance rules](../engineering/performance-rules.md): no decoding, rasterizing or network encoding on the catalog owner or UI thread; no full-frame allocation per pointer movement; explicit queue, participant, bandwidth and memory bounds. Coalesce stale presence and preview updates while preserving durable command results. Render scheduling needs fairness so one guest or an active agent cannot starve the owner or other clients.

Measure host input-to-presented-frame latency separately from remote command acknowledgement and remote frame delivery. Use photo-sized inputs, including RAW, and record network conditions, preview quality, active participants, cache state and peak resources. Select budgets before claiming responsiveness; current local timing results do not establish remote performance.

## Evaluation sequence and acceptance

First establish a useful shared workflow using the existing single-owner service: one photograph, one guest, visible attribution, host-rendered previews and explicit conflict handling. Evaluate gesture sharing, handoff and undo together, because they determine whether the experience feels coherent. This is an experimental scope, not an implementation schedule.

Broaden to more participants, another client type or independent desktop rendering only after that workflow is useful and bounded. Offline replicas, automatic semantic merges and agent proposal branches each require their own demonstrated need and design decision.

Evidence for accepting a future implementation should cover:

- The same allowed edit through the host UI, guest client and agent API produces the same validated recipe and identified render, with unchanged source bytes.
- Independent edits preserve both intentions; same-field, geometry, reset and state-dependent edits follow the selected conflict policy without silently losing work.
- Gestures preview live and commit once; cancelled, superseded and private historical previews never become shared committed state.
- Undo/redo across interleaved participants preserves unrelated work or reports a conflict; Restore has explicit shared consequences.
- Lost acknowledgements, duplicate requests, reconnect, event gaps, host failure, guest departure and revocation preserve durable history and accurately report pending work.
- Invitations enforce asset and operation scope, including host filesystem boundaries, and attributed identity cannot be supplied arbitrarily by a guest.
- Photo-sized measurements demonstrate bounded rendering and transport, fair scheduling and acceptable local/remote latency under recorded network conditions.
- Native host and guest/browser captures correlate displayed pixels with recipe, revision, draft and generation. A headless protocol check alone is not a shared-editing usability or native rendering result.

## Decisions before implementation

Choose the first workflow and guest client; the invitation and connectivity model; view/edit and host-only permissions; the handling of simultaneous gestures and same-field contention; collaborative Undo/Redo and Restore behavior; whether pending drafts survive reconnect; and the preview quality, latency and resource budgets.

These choices should be settled against the small experiment's intended experience. They do not require selecting a CRDT library, adding cloud accounts, changing current catalog compatibility policy or implementing speculative extension points now.

## Research references

- [Figma's published multiplayer architecture](https://www.figma.com/blog/how-figmas-multiplayer-technology-works/) demonstrates an authoritative service with CRDT-inspired property handling and explains why collaborative undo requires its own semantics. It is a precedent, not Lightwell's selected conflict policy.
- [Automerge's conflict semantics](https://automerge.org/docs/reference/documents/conflicts/) illustrate the distinction between deterministic convergence and an application deciding how to present concurrent assignments.
