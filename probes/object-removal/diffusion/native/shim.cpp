// The harness's C ABI over stable-diffusion.cpp: open a context, generate one image at a time
// from an image, a mask and an optional reference, cancel, close; record a text conditioning to
// a file and supply one from a file so a run needs no text encoder. Every entry catches C++
// exceptions and reports failures through a caller-owned message buffer.
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <fstream>
#include <map>
#include <mutex>
#include <string>
#include <vector>

#include "conditioning/conditioner.hpp"
#include "luxforge_condition_hook.h"
#include "stable-diffusion.h"

extern "C" {

typedef struct {
    const char* diffusion_model;
    const char* vae;
    const char* llm;        // optional text encoder
    const char* tokenizer;  // optional tokenizer.json for the text encoder
    int32_t n_threads;      // <= 0: physical cores
    int32_t flash_attn;
    const char* params_backend;  // optional, upstream's assignment string
} lf_sd_open_options;

typedef struct {
    const uint8_t* init_rgb;  // width * height * 3
    const uint8_t* mask;      // width * height, 255 generates, 0 keeps; NULL for none
    uint32_t width;
    uint32_t height;
    const uint8_t* ref_rgb;  // optional reference image, ref_width * ref_height * 3
    uint32_t ref_width;
    uint32_t ref_height;
    const char* prompt;
    const char* lora_path;  // optional
    float lora_multiplier;
    int32_t steps;
    float cfg_scale;
    float strength;
    int64_t seed;
    const char* sample_method;  // optional, upstream's name
} lf_sd_request;

typedef void (*lf_sd_progress_cb)(int32_t step, int32_t steps, float seconds, void* user);

}  // extern "C"

namespace {

std::mutex g_mutex;
std::string g_last_error;
int g_log_level = SD_LOG_WARN;
std::map<std::string, SDCondition> g_observed;
std::map<std::string, SDCondition> g_supplied;
lf_sd_progress_cb g_progress    = nullptr;
bool g_text_encoder_absent      = false;
void* g_progress_user           = nullptr;

void log_callback(enum sd_log_level_t level, const char* text, void*) {
    if (level >= SD_LOG_ERROR) {
        std::lock_guard<std::mutex> lock(g_mutex);
        g_last_error = text;
        while (!g_last_error.empty() && (g_last_error.back() == '\n' || g_last_error.back() == '\r')) {
            g_last_error.pop_back();
        }
    }
    if (static_cast<int>(level) >= g_log_level) {
        std::fputs(text, stderr);
    }
}

void progress_callback(int step, int steps, float time, void*) {
    if (g_progress != nullptr) {
        g_progress(step, steps, time, g_progress_user);
    }
}

void install_callbacks() {
    static std::once_flag once;
    std::call_once(once, [] {
        sd_set_log_callback(log_callback, nullptr);
        sd_set_progress_callback(progress_callback, nullptr);
    });
}

void set_error(char* error, size_t error_len, const std::string& message) {
    if (error == nullptr || error_len == 0) {
        return;
    }
    std::snprintf(error, error_len, "%s", message.c_str());
}

std::string take_last_error(const char* fallback) {
    std::lock_guard<std::mutex> lock(g_mutex);
    std::string message = g_last_error.empty() ? std::string(fallback) : g_last_error;
    g_last_error.clear();
    return message;
}

// The conditioning file: "LFSDCOND", u32 version 1, u32 field count, then per field a u32 name
// length, the name, a u32 rank, i64 dimensions and f32 values. Only the float fields a text
// encoder produces are written; a conditioning with anything else is refused.
constexpr char kMagic[8] = {'L', 'F', 'S', 'D', 'C', 'O', 'N', 'D'};

bool only_float_fields(const SDCondition& c) {
    return c.c_t5_ids.empty() && c.c_t5_weights.empty() && c.c_input_ids.empty() &&
           c.c_position_ids.empty() && c.c_token_types.empty() && c.c_vinput_mask.empty() &&
           c.c_image_embeds.empty() && c.c_ref_images.empty() && c.c_ref_audios.empty() &&
           c.c_reference_blocks.empty() && c.extra_c_crossattns.empty() && c.c_concat.empty();
}

void write_field(std::ofstream& out, const char* name, const sd::Tensor<float>& tensor) {
    uint32_t name_len = static_cast<uint32_t>(std::strlen(name));
    out.write(reinterpret_cast<const char*>(&name_len), 4);
    out.write(name, name_len);
    const auto& shape = tensor.shape();
    uint32_t rank     = static_cast<uint32_t>(shape.size());
    out.write(reinterpret_cast<const char*>(&rank), 4);
    for (int64_t dim : shape) {
        out.write(reinterpret_cast<const char*>(&dim), 8);
    }
    const auto& values = tensor.values();
    out.write(reinterpret_cast<const char*>(values.data()), static_cast<std::streamsize>(values.size() * sizeof(float)));
}

std::string save_condition(const SDCondition& c, const char* path) {
    if (!only_float_fields(c)) {
        return "the recorded conditioning holds fields other than c_crossattn and c_vector";
    }
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    if (!out) {
        return std::string("cannot write ") + path;
    }
    out.write(kMagic, 8);
    uint32_t version = 1;
    uint32_t count   = (c.c_crossattn.empty() ? 0 : 1) + (c.c_vector.empty() ? 0 : 1);
    out.write(reinterpret_cast<const char*>(&version), 4);
    out.write(reinterpret_cast<const char*>(&count), 4);
    if (!c.c_crossattn.empty()) {
        write_field(out, "c_crossattn", c.c_crossattn);
    }
    if (!c.c_vector.empty()) {
        write_field(out, "c_vector", c.c_vector);
    }
    out.close();
    if (!out) {
        return std::string("failed writing ") + path;
    }
    return "";
}

std::string load_condition(const char* path, SDCondition& c) {
    std::ifstream in(path, std::ios::binary);
    if (!in) {
        return std::string("cannot read ") + path;
    }
    char magic[8];
    uint32_t version = 0;
    uint32_t count   = 0;
    in.read(magic, 8);
    in.read(reinterpret_cast<char*>(&version), 4);
    in.read(reinterpret_cast<char*>(&count), 4);
    if (!in || std::memcmp(magic, kMagic, 8) != 0 || version != 1 || count > 2) {
        return std::string(path) + " is not a version 1 conditioning file";
    }
    for (uint32_t i = 0; i < count; i++) {
        uint32_t name_len = 0;
        in.read(reinterpret_cast<char*>(&name_len), 4);
        if (!in || name_len > 64) {
            return std::string(path) + ": bad field name";
        }
        std::string name(name_len, '\0');
        in.read(name.data(), name_len);
        uint32_t rank = 0;
        in.read(reinterpret_cast<char*>(&rank), 4);
        if (!in || rank == 0 || rank > 8) {
            return std::string(path) + ": bad rank";
        }
        std::vector<int64_t> shape(rank);
        int64_t elements = 1;
        for (auto& dim : shape) {
            in.read(reinterpret_cast<char*>(&dim), 8);
            if (!in || dim <= 0 || dim > (1 << 24)) {
                return std::string(path) + ": bad dimension";
            }
            elements *= dim;
        }
        if (elements > (int64_t(1) << 28)) {
            return std::string(path) + ": field too large";
        }
        std::vector<float> values(static_cast<size_t>(elements));
        in.read(reinterpret_cast<char*>(values.data()), static_cast<std::streamsize>(values.size() * sizeof(float)));
        if (!in) {
            return std::string(path) + ": truncated";
        }
        sd::Tensor<float> tensor(shape, std::move(values));
        if (name == "c_crossattn") {
            c.c_crossattn = std::move(tensor);
        } else if (name == "c_vector") {
            c.c_vector = std::move(tensor);
        } else {
            return std::string(path) + ": unknown field " + name;
        }
    }
    if (in.peek() != std::char_traits<char>::eof()) {
        return std::string(path) + ": trailing bytes";
    }
    return "";
}

}  // namespace

bool luxforge_condition_supplied(const std::string& text, SDCondition& out) {
    std::lock_guard<std::mutex> lock(g_mutex);
    auto found = g_supplied.find(text);
    if (found == g_supplied.end()) {
        if (g_text_encoder_absent) {
            throw std::runtime_error("no text encoder is loaded and no conditioning was supplied for the prompt \"" + text + "\"");
        }
        return false;
    }
    out = found->second;
    return true;
}

bool luxforge_text_encoder_absent() {
    return g_text_encoder_absent;
}

void luxforge_condition_observed(const std::string& text, const SDCondition& condition) {
    std::lock_guard<std::mutex> lock(g_mutex);
    g_observed[text] = condition;
}

extern "C" {

void lf_sd_set_log_level(int32_t level) {
    g_log_level = level;
    install_callbacks();
}

void lf_sd_set_progress(lf_sd_progress_cb cb, void* user) {
    install_callbacks();
    std::lock_guard<std::mutex> lock(g_mutex);
    g_progress      = cb;
    g_progress_user = user;
}

const char* lf_sd_system_info(void) {
    return sd_get_system_info();
}

sd_ctx_t* lf_sd_open(const lf_sd_open_options* options, char* error, size_t error_len) {
    install_callbacks();
    try {
        // One context at a time in this harness: the flag describes the context being opened.
        g_text_encoder_absent = options->llm == nullptr || options->llm[0] == '\0';
        sd_ctx_params_t params;
        sd_ctx_params_init(&params);
        params.diffusion_model_path = options->diffusion_model;
        params.vae_path             = options->vae;
        params.llm_path             = options->llm;
        params.tokenizer            = options->tokenizer;
        params.n_threads            = options->n_threads > 0 ? options->n_threads : sd_get_num_physical_cores();
        params.diffusion_flash_attn = options->flash_attn != 0;
        params.flash_attn           = options->flash_attn != 0;
        if (options->params_backend != nullptr) {
            params.params_backend = options->params_backend;
        }
        sd_ctx_t* ctx = new_sd_ctx(&params);
        if (ctx == nullptr) {
            set_error(error, error_len, take_last_error("new_sd_ctx failed"));
        }
        return ctx;
    } catch (const std::exception& e) {
        set_error(error, error_len, e.what());
    } catch (...) {
        set_error(error, error_len, "unknown exception in lf_sd_open");
    }
    return nullptr;
}

void lf_sd_close(sd_ctx_t* ctx) {
    if (ctx != nullptr) {
        free_sd_ctx(ctx);
    }
}

void lf_sd_cancel(sd_ctx_t* ctx) {
    if (ctx != nullptr) {
        sd_cancel_generation(ctx, SD_CANCEL_ALL);
    }
}

const char* lf_sd_model_version(const sd_ctx_t* ctx) {
    return sd_get_model_version_name(ctx);
}

int32_t lf_sd_generate(sd_ctx_t* ctx, const lf_sd_request* request, uint8_t* out_rgb, char* error, size_t error_len) {
    try {
        sd_cancel_generation(ctx, SD_CANCEL_RESET);
        std::vector<uint8_t> init(request->init_rgb, request->init_rgb + size_t(request->width) * request->height * 3);
        std::vector<uint8_t> mask;
        if (request->mask != nullptr) {
            mask.assign(request->mask, request->mask + size_t(request->width) * request->height);
        }
        std::vector<uint8_t> ref;
        sd_image_t ref_image{};
        if (request->ref_rgb != nullptr) {
            ref.assign(request->ref_rgb, request->ref_rgb + size_t(request->ref_width) * request->ref_height * 3);
            ref_image = sd_image_t{request->ref_width, request->ref_height, 3, ref.data()};
        }
        sd_lora_t lora{};
        sd_img_gen_params_t params;
        sd_img_gen_params_init(&params);
        if (request->lora_path != nullptr) {
            lora.is_high_noise = false;
            lora.multiplier    = request->lora_multiplier;
            lora.path          = request->lora_path;
            params.loras       = &lora;
            params.lora_count  = 1;
        }
        params.prompt          = request->prompt != nullptr ? request->prompt : "";
        params.negative_prompt = "";
        params.init_image      = sd_image_t{request->width, request->height, 3, init.data()};
        if (!mask.empty()) {
            params.mask_image = sd_image_t{request->width, request->height, 1, mask.data()};
        }
        if (!ref.empty()) {
            params.ref_images       = &ref_image;
            params.ref_images_count = 1;
        }
        params.width                                = static_cast<int>(request->width);
        params.height                               = static_cast<int>(request->height);
        params.strength                             = request->strength;
        params.seed                                 = request->seed;
        params.batch_count                          = 1;
        params.sample_params.sample_steps           = request->steps;
        params.sample_params.guidance.txt_cfg       = request->cfg_scale;
        if (request->sample_method != nullptr) {
            params.sample_params.sample_method = str_to_sample_method(request->sample_method);
        }
        sd_image_t* images = nullptr;
        int count          = 0;
        bool ok            = generate_image(ctx, &params, &images, &count);
        if (!ok || images == nullptr || count < 1) {
            set_error(error, error_len, take_last_error("generate_image produced no image"));
            if (images != nullptr) {
                for (int i = 0; i < count; i++) {
                    std::free(images[i].data);
                }
                std::free(images);
            }
            return 1;
        }
        int32_t status = 0;
        if (images[0].width != request->width || images[0].height != request->height || images[0].channel != 3) {
            set_error(error, error_len, "generate_image returned an image of another size or format");
            status = 1;
        } else {
            std::memcpy(out_rgb, images[0].data, size_t(request->width) * request->height * 3);
        }
        for (int i = 0; i < count; i++) {
            std::free(images[i].data);
        }
        std::free(images);
        return status;
    } catch (const std::exception& e) {
        set_error(error, error_len, e.what());
    } catch (...) {
        set_error(error, error_len, "unknown exception in lf_sd_generate");
    }
    return 1;
}

// Write the conditioning the text encoder last computed for `text` to `path`.
int32_t lf_sd_condition_save(const char* text, const char* path, char* error, size_t error_len) {
    try {
        SDCondition condition;
        {
            std::lock_guard<std::mutex> lock(g_mutex);
            auto found = g_observed.find(text);
            if (found == g_observed.end()) {
                set_error(error, error_len, "no conditioning was computed for that text");
                return 1;
            }
            condition = found->second;
        }
        std::string message = save_condition(condition, path);
        if (!message.empty()) {
            set_error(error, error_len, message);
            return 1;
        }
        return 0;
    } catch (const std::exception& e) {
        set_error(error, error_len, e.what());
    }
    return 1;
}

// Supply the conditioning in `path` for `text`, or with a NULL path stop supplying one.
int32_t lf_sd_condition_supply(const char* text, const char* path, char* error, size_t error_len) {
    try {
        if (path == nullptr) {
            std::lock_guard<std::mutex> lock(g_mutex);
            g_supplied.erase(text);
            return 0;
        }
        SDCondition condition;
        std::string message = load_condition(path, condition);
        if (!message.empty()) {
            set_error(error, error_len, message);
            return 1;
        }
        std::lock_guard<std::mutex> lock(g_mutex);
        g_supplied[text] = std::move(condition);
        return 0;
    } catch (const std::exception& e) {
        set_error(error, error_len, e.what());
    }
    return 1;
}

}  // extern "C"
