// Replaces stable-diffusion.cpp's src/tokenizers/vocab/vocab.cpp. Upstream compiles 139 MB of
// generated vocabulary tables into the library; this build bundles none of them. A text encoder
// is given its tokenizer as a file (sd_ctx_params_t::tokenizer, a Hugging Face tokenizer.json).
// A context opened without a text encoder still constructs its conditioner, and with it a
// tokenizer it never uses (every prompt then needs a supplied conditioning, which the hook in
// shim.cpp enforces): for that context the tables are a one-merge stub.
#include "tokenizers/vocab/vocab.h"

#include <stdexcept>

#include "luxforge_condition_hook.h"

namespace {
std::string not_bundled(const char* table) {
    if (luxforge_text_encoder_absent()) {
        // A valid, never-used table: one merge line, or an empty JSON vocabulary.
        return std::string(table).find("vocabulary") != std::string::npos ? "{}" : "a b";
    }
    throw std::runtime_error(std::string("the embedded ") + table +
                             " tokenizer table is not bundled in this build; pass a tokenizer.json");
}
}  // namespace

std::string load_clip_merges() { return not_bundled("CLIP merges"); }
std::string load_qwen2_merges() { return not_bundled("Qwen2 merges"); }
std::string load_mistral_merges() { return not_bundled("Mistral merges"); }
std::string load_mistral_vocab_json() { return not_bundled("Mistral vocabulary"); }
std::string load_t5_tokenizer_json() { return not_bundled("T5 vocabulary"); }
std::string load_umt5_tokenizer_json() { return not_bundled("UMT5 vocabulary"); }
std::string load_gemma_merges() { return not_bundled("Gemma merges"); }
std::string load_gemma_vocab_json() { return not_bundled("Gemma vocabulary"); }
