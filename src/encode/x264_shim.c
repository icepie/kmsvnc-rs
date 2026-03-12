#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

#include <x264.h>

struct kmsvnc_x264_encoder {
    x264_t *enc;
    x264_picture_t pic_in;
    x264_picture_t pic_out;
    int width;
    int height;
};

static char kmsvnc_x264_error[256];

static void set_errorf(const char *msg) {
    snprintf(kmsvnc_x264_error, sizeof(kmsvnc_x264_error), "%s", msg);
}

const char *kmsvnc_x264_last_error(void) {
    return kmsvnc_x264_error;
}

struct kmsvnc_x264_encoder *kmsvnc_x264_open(uint32_t width, uint32_t height) {
    struct kmsvnc_x264_encoder *ctx = calloc(1, sizeof(*ctx));
    if (!ctx) {
        set_errorf("x264 encoder alloc failed");
        return NULL;
    }

    x264_param_t param;
    if (x264_param_default_preset(&param, "ultrafast", "zerolatency") < 0) {
        set_errorf("x264_param_default_preset failed");
        free(ctx);
        return NULL;
    }

    param.i_csp = X264_CSP_I420;
    param.i_width = (int)width;
    param.i_height = (int)height;
    param.i_fps_num = 30;
    param.i_fps_den = 1;
    param.i_keyint_max = 30;
    param.b_repeat_headers = 1;
    param.b_vfr_input = 0;
    param.b_annexb = 1;
    param.rc.i_rc_method = X264_RC_CRF;
    param.rc.f_rf_constant = 28.0f;
    param.rc.f_rf_constant_max = 35.0f;
    param.i_threads = 1;

    if (x264_param_apply_profile(&param, "baseline") < 0) {
        set_errorf("x264_param_apply_profile failed");
        free(ctx);
        return NULL;
    }

    ctx->enc = x264_encoder_open(&param);
    if (!ctx->enc) {
        set_errorf("x264_encoder_open failed");
        free(ctx);
        return NULL;
    }

    if (x264_picture_alloc(&ctx->pic_in, X264_CSP_I420, (int)width, (int)height) < 0) {
        set_errorf("x264_picture_alloc failed");
        x264_encoder_close(ctx->enc);
        free(ctx);
        return NULL;
    }

    ctx->width = (int)width;
    ctx->height = (int)height;
    return ctx;
}

void kmsvnc_x264_close(struct kmsvnc_x264_encoder *ctx) {
    if (!ctx) {
        return;
    }
    x264_picture_clean(&ctx->pic_in);
    if (ctx->enc) {
        x264_encoder_close(ctx->enc);
    }
    free(ctx);
}

static inline uint8_t clip_u8(int v) {
    if (v < 0) {
        return 0;
    }
    if (v > 255) {
        return 255;
    }
    return (uint8_t)v;
}

static void bgra_to_i420(struct kmsvnc_x264_encoder *ctx, const uint8_t *bgra, uint32_t stride) {
    uint8_t *y_plane = ctx->pic_in.img.plane[0];
    uint8_t *u_plane = ctx->pic_in.img.plane[1];
    uint8_t *v_plane = ctx->pic_in.img.plane[2];
    int y_stride = ctx->pic_in.img.i_stride[0];
    int u_stride = ctx->pic_in.img.i_stride[1];
    int v_stride = ctx->pic_in.img.i_stride[2];

    for (int y = 0; y < ctx->height; y += 2) {
        const uint8_t *row0 = bgra + (size_t)y * stride;
        const uint8_t *row1 = bgra + (size_t)(y + 1 < ctx->height ? y + 1 : y) * stride;
        uint8_t *y0 = y_plane + y * y_stride;
        uint8_t *y1 = y_plane + (y + 1 < ctx->height ? y + 1 : y) * y_stride;
        uint8_t *u = u_plane + (y / 2) * u_stride;
        uint8_t *v = v_plane + (y / 2) * v_stride;

        for (int x = 0; x < ctx->width; x += 2) {
            int b[4], g[4], r[4];
            const uint8_t *p[4] = {
                row0 + x * 4,
                row0 + (x + 1 < ctx->width ? x + 1 : x) * 4,
                row1 + x * 4,
                row1 + (x + 1 < ctx->width ? x + 1 : x) * 4,
            };

            for (int i = 0; i < 4; i++) {
                b[i] = p[i][0];
                g[i] = p[i][1];
                r[i] = p[i][2];
            }

            y0[x] = clip_u8(((66 * r[0] + 129 * g[0] + 25 * b[0] + 128) >> 8) + 16);
            if (x + 1 < ctx->width) {
                y0[x + 1] = clip_u8(((66 * r[1] + 129 * g[1] + 25 * b[1] + 128) >> 8) + 16);
            }
            if (y + 1 < ctx->height) {
                y1[x] = clip_u8(((66 * r[2] + 129 * g[2] + 25 * b[2] + 128) >> 8) + 16);
                if (x + 1 < ctx->width) {
                    y1[x + 1] = clip_u8(((66 * r[3] + 129 * g[3] + 25 * b[3] + 128) >> 8) + 16);
                }
            }

            int r_avg = (r[0] + r[1] + r[2] + r[3]) / 4;
            int g_avg = (g[0] + g[1] + g[2] + g[3]) / 4;
            int b_avg = (b[0] + b[1] + b[2] + b[3]) / 4;

            u[x / 2] = clip_u8(((-38 * r_avg - 74 * g_avg + 112 * b_avg + 128) >> 8) + 128);
            v[x / 2] = clip_u8(((112 * r_avg - 94 * g_avg - 18 * b_avg + 128) >> 8) + 128);
        }
    }
}

int kmsvnc_x264_encode(
    struct kmsvnc_x264_encoder *ctx,
    const uint8_t *bgra,
    uint32_t stride,
    uint64_t pts,
    int force_keyframe,
    uint8_t **out_data,
    size_t *out_len,
    int *out_keyframe
) {
    if (!ctx || !bgra || !out_data || !out_len || !out_keyframe) {
        set_errorf("invalid x264 encode arguments");
        return 0;
    }

    bgra_to_i420(ctx, bgra, stride);
    ctx->pic_in.i_pts = (int64_t)pts;
    ctx->pic_in.i_type = force_keyframe ? X264_TYPE_IDR : X264_TYPE_AUTO;

    x264_nal_t *nals = NULL;
    int i_nals = 0;
    int frame_size = x264_encoder_encode(ctx->enc, &nals, &i_nals, &ctx->pic_in, &ctx->pic_out);
    if (frame_size < 0) {
        set_errorf("x264_encoder_encode failed");
        return 0;
    }

    if (frame_size == 0 || i_nals <= 0) {
        *out_data = NULL;
        *out_len = 0;
        *out_keyframe = 0;
        return 1;
    }

    uint8_t *buf = malloc((size_t)frame_size);
    if (!buf) {
        set_errorf("x264 output alloc failed");
        return 0;
    }

    size_t offset = 0;
    for (int i = 0; i < i_nals; i++) {
        memcpy(buf + offset, nals[i].p_payload, (size_t)nals[i].i_payload);
        offset += (size_t)nals[i].i_payload;
    }

    *out_data = buf;
    *out_len = offset;
    *out_keyframe = (ctx->pic_out.b_keyframe != 0);
    return 1;
}

void kmsvnc_x264_free_packet(uint8_t *data) {
    free(data);
}
