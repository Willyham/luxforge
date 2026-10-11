// The local hook around StableDiffusionGGML::get_learned_condition (build.rs stages the one
// patched file; THIRD_PARTY.md records the change). Defined in shim.cpp.
#pragma once

#include <string>

struct SDCondition;

// When the shim holds a supplied conditioning, write it to `out` and answer true; the text
// encoder is then not run.
bool luxforge_condition_supplied(const std::string& text, SDCondition& out);
// Called with every conditioning the text encoder computes.
void luxforge_condition_observed(const std::string& text, const SDCondition& condition);
// Whether the context being opened has no text encoder: its tokenizer is then never used, and
// every prompt must have a supplied conditioning.
bool luxforge_text_encoder_absent();
