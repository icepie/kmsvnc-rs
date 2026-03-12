#define _GNU_SOURCE

#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <xf86drm.h>
#include <va/va.h>
#include <va/va_enc_h264.h>
#include <va/va_drm.h>
#include <va/va_drmcommon.h>
#include <va/va_vpp.h>

struct kmsvnc_vaapi {
    VADisplay dpy;
    VASurfaceID surface;
    VAImage image;
    void *imgbuf;
    int prime_fd;
    int display_fd;
    int output_is_bgra;
    uint32_t src_x;
    uint32_t src_y;
    uint32_t width;
    uint32_t height;
    uint32_t fb_width;
    uint32_t fb_height;
};

struct kmsvnc_vaapi_encoder {
    VADisplay dpy;
    VASurfaceID surface;
    VASurfaceID vpp_output_surface;
    VAConfigID vpp_config;
    VAContextID vpp_context;
    int display_fd;
    int object_fds[4];
    uint32_t width;
    uint32_t height;
    int supports_h264;
    int supports_h264_low_power;
    int supports_vpp;
    int supports_vpp_nv12;
};

static char kmsvnc_vaapi_error[256];

static void set_errorf(const char *fmt, const char *detail) {
    if (detail) {
        snprintf(kmsvnc_vaapi_error, sizeof(kmsvnc_vaapi_error), fmt, detail);
    } else {
        snprintf(kmsvnc_vaapi_error, sizeof(kmsvnc_vaapi_error), "%s", fmt);
    }
}

static uint32_t fourcc_code(char a, char b, char c, char d) {
    return ((uint32_t)(uint8_t)a) |
           (((uint32_t)(uint8_t)b) << 8) |
           (((uint32_t)(uint8_t)c) << 16) |
           (((uint32_t)(uint8_t)d) << 24);
}

static int va_ok(VAStatus status, const char *op) {
    if (status == VA_STATUS_SUCCESS) {
        return 1;
    }
    snprintf(
        kmsvnc_vaapi_error,
        sizeof(kmsvnc_vaapi_error),
        "%s failed: %s",
        op,
        vaErrorStr(status)
    );
    return 0;
}

const char *kmsvnc_vaapi_last_error(void) {
    return kmsvnc_vaapi_error;
}

void kmsvnc_vaapi_close(struct kmsvnc_vaapi *ctx) {
    if (!ctx) {
        return;
    }
    if (ctx->imgbuf) {
        vaUnmapBuffer(ctx->dpy, ctx->image.buf);
    }
    if (ctx->image.image_id != VA_INVALID_ID) {
        vaDestroyImage(ctx->dpy, ctx->image.image_id);
    }
    if (ctx->surface != VA_INVALID_ID) {
        vaDestroySurfaces(ctx->dpy, &ctx->surface, 1);
    }
    if (ctx->dpy) {
        vaTerminate(ctx->dpy);
    }
    if (ctx->display_fd >= 0) {
        close(ctx->display_fd);
    }
    if (ctx->prime_fd >= 0) {
        close(ctx->prime_fd);
    }
    free(ctx);
}

void kmsvnc_vaapi_encoder_close(struct kmsvnc_vaapi_encoder *ctx) {
    if (!ctx) {
        return;
    }
    if (ctx->vpp_context != VA_INVALID_ID) {
        vaDestroyContext(ctx->dpy, ctx->vpp_context);
    }
    if (ctx->vpp_config != VA_INVALID_ID) {
        vaDestroyConfig(ctx->dpy, ctx->vpp_config);
    }
    if (ctx->vpp_output_surface != VA_INVALID_ID) {
        vaDestroySurfaces(ctx->dpy, &ctx->vpp_output_surface, 1);
    }
    if (ctx->surface != VA_INVALID_ID) {
        vaDestroySurfaces(ctx->dpy, &ctx->surface, 1);
    }
    if (ctx->dpy) {
        vaTerminate(ctx->dpy);
    }
    if (ctx->display_fd >= 0) {
        close(ctx->display_fd);
    }
    for (int i = 0; i < 4; i++) {
        if (ctx->object_fds[i] >= 0) {
            close(ctx->object_fds[i]);
        }
    }
    free(ctx);
}

static int kmsvnc_vaapi_open_display(int drm_fd, VADisplay *out_dpy, int *out_display_fd) {
    char *render_node = drmGetRenderDeviceNameFromFd(drm_fd);
    int display_fd = -1;
    if (render_node) {
        display_fd = open(render_node, O_RDWR);
        free(render_node);
    }
    if (display_fd < 0) {
        display_fd = dup(drm_fd);
    }
    if (display_fd < 0) {
        set_errorf("failed to open DRM/render node for VAAPI", NULL);
        return 0;
    }

    VADisplay dpy = vaGetDisplayDRM(display_fd);
    if (!dpy) {
        close(display_fd);
        set_errorf("vaGetDisplayDRM failed", NULL);
        return 0;
    }

    int major = 0;
    int minor = 0;
    if (!va_ok(vaInitialize(dpy, &major, &minor), "vaInitialize")) {
        close(display_fd);
        return 0;
    }

    *out_dpy = dpy;
    *out_display_fd = display_fd;
    return 1;
}

static int kmsvnc_vaapi_import_prime_surface(
    struct kmsvnc_vaapi_encoder *ctx,
    uint32_t width,
    uint32_t height,
    uint32_t drm_format,
    uint64_t modifier,
    const int *object_fds,
    const uint64_t *object_sizes,
    uint32_t num_objects,
    const uint32_t *object_indices,
    const uint32_t *offsets,
    const uint32_t *pitches,
    uint32_t num_planes,
    uint32_t usage_hint
) {
    VADRMPRIMESurfaceDescriptor desc;
    memset(&desc, 0, sizeof(desc));
    desc.width = width;
    desc.height = height;
    desc.fourcc = fourcc_code('B', 'G', 'R', 'X');
    desc.num_objects = num_objects;
    desc.num_layers = 1;
    desc.layers[0].drm_format = drm_format;
    desc.layers[0].num_planes = num_planes;

    for (uint32_t i = 0; i < num_objects && i < 4; i++) {
        ctx->object_fds[i] = dup(object_fds[i]);
        if (ctx->object_fds[i] < 0) {
            set_errorf("dup(object_fd) failed", NULL);
            return 0;
        }
        desc.objects[i].fd = ctx->object_fds[i];
        desc.objects[i].size = object_sizes[i];
        desc.objects[i].drm_format_modifier = modifier;
    }

    for (uint32_t i = 0; i < num_planes && i < 4; i++) {
        desc.layers[0].object_index[i] = object_indices[i];
        desc.layers[0].offset[i] = offsets[i];
        desc.layers[0].pitch[i] = pitches[i];
    }

    VASurfaceAttrib attrs[3];
    memset(attrs, 0, sizeof(attrs));
    attrs[0].type = VASurfaceAttribMemoryType;
    attrs[0].flags = VA_SURFACE_ATTRIB_SETTABLE;
    attrs[0].value.type = VAGenericValueTypeInteger;
    attrs[0].value.value.i = VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2;
    attrs[1].type = VASurfaceAttribExternalBufferDescriptor;
    attrs[1].flags = VA_SURFACE_ATTRIB_SETTABLE;
    attrs[1].value.type = VAGenericValueTypePointer;
    attrs[1].value.value.p = &desc;
    attrs[2].type = VASurfaceAttribUsageHint;
    attrs[2].flags = VA_SURFACE_ATTRIB_SETTABLE;
    attrs[2].value.type = VAGenericValueTypeInteger;
    attrs[2].value.value.i = usage_hint;

    if (!va_ok(
            vaCreateSurfaces(
                ctx->dpy,
                VA_RT_FORMAT_RGB32,
                width,
                height,
                &ctx->surface,
                1,
                attrs,
                3),
            "vaCreateSurfaces")) {
        return 0;
    }
    return 1;
}

static void kmsvnc_vaapi_probe_vpp_nv12(struct kmsvnc_vaapi_encoder *ctx) {
    VAEntrypoint entrypoints[16];
    int num_entrypoints = 0;
    if (!va_ok(
            vaQueryConfigEntrypoints(ctx->dpy, VAProfileNone, entrypoints, &num_entrypoints),
            "vaQueryConfigEntrypoints")) {
        return;
    }

    int has_vpp = 0;
    for (int i = 0; i < num_entrypoints; i++) {
        if (entrypoints[i] == VAEntrypointVideoProc) {
            has_vpp = 1;
            break;
        }
    }
    if (!has_vpp) {
        return;
    }
    ctx->supports_vpp = 1;

    if (!va_ok(
            vaCreateConfig(ctx->dpy, VAProfileNone, VAEntrypointVideoProc, NULL, 0, &ctx->vpp_config),
            "vaCreateConfig(VideoProc)")) {
        return;
    }

    VASurfaceAttrib out_attrs[2];
    memset(out_attrs, 0, sizeof(out_attrs));
    out_attrs[0].type = VASurfaceAttribPixelFormat;
    out_attrs[0].flags = VA_SURFACE_ATTRIB_SETTABLE;
    out_attrs[0].value.type = VAGenericValueTypeInteger;
    out_attrs[0].value.value.i = VA_FOURCC_NV12;
    out_attrs[1].type = VASurfaceAttribUsageHint;
    out_attrs[1].flags = VA_SURFACE_ATTRIB_SETTABLE;
    out_attrs[1].value.type = VAGenericValueTypeInteger;
    out_attrs[1].value.value.i = VA_SURFACE_ATTRIB_USAGE_HINT_VPP_WRITE | VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER;

    if (!va_ok(
            vaCreateSurfaces(
                ctx->dpy,
                VA_RT_FORMAT_YUV420,
                ctx->width,
                ctx->height,
                &ctx->vpp_output_surface,
                1,
                out_attrs,
                2),
            "vaCreateSurfaces(NV12 VPP output)")) {
        return;
    }

    if (!va_ok(
            vaCreateContext(
                ctx->dpy,
                ctx->vpp_config,
                ctx->width,
                ctx->height,
                VA_PROGRESSIVE,
                &ctx->vpp_output_surface,
                1,
                &ctx->vpp_context),
            "vaCreateContext(VideoProc)")) {
        return;
    }

    ctx->supports_vpp_nv12 = 1;
}

struct kmsvnc_vaapi_encoder *kmsvnc_vaapi_encoder_open(
    int drm_fd,
    uint32_t width,
    uint32_t height,
    uint32_t drm_format,
    uint64_t modifier,
    const int *object_fds,
    const uint64_t *object_sizes,
    uint32_t num_objects,
    const uint32_t *object_indices,
    const uint32_t *offsets,
    const uint32_t *pitches,
    uint32_t num_planes
) {
    struct kmsvnc_vaapi_encoder *ctx = calloc(1, sizeof(*ctx));
    if (!ctx) {
        set_errorf("out of memory", NULL);
        return NULL;
    }

    ctx->surface = VA_INVALID_ID;
    ctx->vpp_output_surface = VA_INVALID_ID;
    ctx->vpp_config = VA_INVALID_ID;
    ctx->vpp_context = VA_INVALID_ID;
    ctx->display_fd = -1;
    for (int i = 0; i < 4; i++) {
        ctx->object_fds[i] = -1;
    }

    if (!kmsvnc_vaapi_open_display(drm_fd, &ctx->dpy, &ctx->display_fd)) {
        kmsvnc_vaapi_encoder_close(ctx);
        return NULL;
    }

    VAEntrypoint entrypoints[16];
    int num_entrypoints = 0;
    if (va_ok(
            vaQueryConfigEntrypoints(ctx->dpy, VAProfileH264Main, entrypoints, &num_entrypoints),
            "vaQueryConfigEntrypoints")) {
        for (int i = 0; i < num_entrypoints; i++) {
            if (entrypoints[i] == VAEntrypointEncSlice) {
                ctx->supports_h264 = 1;
            } else if (entrypoints[i] == VAEntrypointEncSliceLP) {
                ctx->supports_h264 = 1;
                ctx->supports_h264_low_power = 1;
            }
        }
    }

    if (!kmsvnc_vaapi_import_prime_surface(
            ctx,
            width,
            height,
            drm_format,
            modifier,
            object_fds,
            object_sizes,
            num_objects,
            object_indices,
            offsets,
            pitches,
            num_planes,
            VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER | VA_SURFACE_ATTRIB_USAGE_HINT_VPP_READ)) {
        kmsvnc_vaapi_encoder_close(ctx);
        return NULL;
    }

    ctx->width = width;
    ctx->height = height;
    kmsvnc_vaapi_probe_vpp_nv12(ctx);
    fprintf(stderr,
        "kmsvnc vaapi encoder probe: import_ok=1 h264=%d low_power=%d vpp=%d vpp_nv12=%d size=%ux%u format=%u\n",
        ctx->supports_h264,
        ctx->supports_h264_low_power,
        ctx->supports_vpp,
        ctx->supports_vpp_nv12,
        width,
        height,
        drm_format);
    return ctx;
}

int kmsvnc_vaapi_encoder_supports_h264(const struct kmsvnc_vaapi_encoder *ctx) {
    return ctx && ctx->supports_h264;
}

struct kmsvnc_vaapi *kmsvnc_vaapi_open(
    int drm_fd,
    int prime_fd,
    uint32_t src_x,
    uint32_t src_y,
    uint32_t width,
    uint32_t height,
    uint32_t fb_width,
    uint32_t fb_height,
    uint32_t drm_format,
    uint64_t modifier,
    const uint32_t *pitches,
    const uint32_t *offsets,
    uint32_t num_planes
) {
    struct kmsvnc_vaapi *ctx = calloc(1, sizeof(*ctx));
    if (!ctx) {
        set_errorf("out of memory", NULL);
        return NULL;
    }

    ctx->surface = VA_INVALID_ID;
    ctx->image.image_id = VA_INVALID_ID;
    ctx->display_fd = -1;
    ctx->prime_fd = dup(prime_fd);
    if (ctx->prime_fd < 0) {
        set_errorf("dup(prime_fd) failed", NULL);
        free(ctx);
        return NULL;
    }

    char *render_node = drmGetRenderDeviceNameFromFd(drm_fd);
    int display_fd = -1;
    if (render_node) {
        display_fd = open(render_node, O_RDWR);
        free(render_node);
    }
    if (display_fd < 0) {
        display_fd = dup(drm_fd);
    }
    if (display_fd < 0) {
        set_errorf("failed to open DRM/render node for VAAPI", NULL);
        kmsvnc_vaapi_close(ctx);
        return NULL;
    }
    ctx->display_fd = display_fd;

    ctx->dpy = vaGetDisplayDRM(display_fd);
    if (!ctx->dpy) {
        set_errorf("vaGetDisplayDRM failed", NULL);
        kmsvnc_vaapi_close(ctx);
        return NULL;
    }

    int major = 0;
    int minor = 0;
    if (!va_ok(vaInitialize(ctx->dpy, &major, &minor), "vaInitialize")) {
        kmsvnc_vaapi_close(ctx);
        return NULL;
    }

    VADRMPRIMESurfaceDescriptor desc;
    memset(&desc, 0, sizeof(desc));
    desc.fourcc = fourcc_code('B', 'G', 'R', 'X');
    desc.width = fb_width;
    desc.height = fb_height;
    desc.num_objects = 1;
    desc.objects[0].fd = ctx->prime_fd;
    desc.objects[0].drm_format_modifier = modifier;
    desc.num_layers = 1;
    desc.layers[0].drm_format = drm_format;
    desc.layers[0].num_planes = num_planes;

    uint32_t max_size = 0;
    for (uint32_t i = 0; i < num_planes; i++) {
        uint32_t end = offsets[i] + pitches[i] * fb_height;
        if (end > max_size) {
            max_size = end;
        }
        desc.layers[0].object_index[i] = 0;
        desc.layers[0].offset[i] = offsets[i];
        desc.layers[0].pitch[i] = pitches[i];
    }
    desc.objects[0].size = max_size;

    VASurfaceAttrib attrs[2];
    memset(attrs, 0, sizeof(attrs));
    attrs[0].type = VASurfaceAttribMemoryType;
    attrs[0].flags = VA_SURFACE_ATTRIB_SETTABLE;
    attrs[0].value.type = VAGenericValueTypeInteger;
    attrs[0].value.value.i = VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2;
    attrs[1].type = VASurfaceAttribExternalBufferDescriptor;
    attrs[1].flags = VA_SURFACE_ATTRIB_SETTABLE;
    attrs[1].value.type = VAGenericValueTypePointer;
    attrs[1].value.value.p = &desc;

    VAStatus status = vaCreateSurfaces(
        ctx->dpy,
        VA_RT_FORMAT_RGB32,
        fb_width,
        fb_height,
        &ctx->surface,
        1,
        attrs,
        2
    );
    if (status != VA_STATUS_SUCCESS) {
        VAStatus prime2_status = status;
        VASurfaceAttribExternalBuffers buffer_desc;
        memset(&buffer_desc, 0, sizeof(buffer_desc));
        uintptr_t buffer = (uintptr_t)ctx->prime_fd;
        buffer_desc.pixel_format = desc.fourcc;
        buffer_desc.width = fb_width;
        buffer_desc.height = fb_height;
        buffer_desc.data_size = max_size;
        buffer_desc.num_planes = num_planes;
        buffer_desc.buffers = &buffer;
        buffer_desc.num_buffers = 1;
        for (uint32_t i = 0; i < num_planes; i++) {
            buffer_desc.pitches[i] = pitches[i];
            buffer_desc.offsets[i] = offsets[i];
        }

        VASurfaceAttrib fallback_attrs[2];
        memset(fallback_attrs, 0, sizeof(fallback_attrs));
        fallback_attrs[0].type = VASurfaceAttribMemoryType;
        fallback_attrs[0].flags = VA_SURFACE_ATTRIB_SETTABLE;
        fallback_attrs[0].value.type = VAGenericValueTypeInteger;
        fallback_attrs[0].value.value.i = VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME;
        fallback_attrs[1].type = VASurfaceAttribExternalBufferDescriptor;
        fallback_attrs[1].flags = VA_SURFACE_ATTRIB_SETTABLE;
        fallback_attrs[1].value.type = VAGenericValueTypePointer;
        fallback_attrs[1].value.value.p = &buffer_desc;

        status = vaCreateSurfaces(
            ctx->dpy,
            VA_RT_FORMAT_RGB32,
            fb_width,
            fb_height,
            &ctx->surface,
            1,
            fallback_attrs,
            2
        );
        if (status != VA_STATUS_SUCCESS) {
            snprintf(
                kmsvnc_vaapi_error,
                sizeof(kmsvnc_vaapi_error),
                "vaCreateSurfaces failed: prime2=%s, prime=%s",
                vaErrorStr(prime2_status),
                vaErrorStr(status)
            );
            kmsvnc_vaapi_close(ctx);
            return NULL;
        }
    }

    VAImageFormat formats[64];
    int n_formats = 0;
    if (!va_ok(vaQueryImageFormats(ctx->dpy, formats, &n_formats), "vaQueryImageFormats")) {
        kmsvnc_vaapi_close(ctx);
        return NULL;
    }

    VAImageFormat *selected = NULL;
    uint32_t preferred[] = {
        VA_FOURCC_BGRA,
        VA_FOURCC_BGRX,
    };
    for (size_t p = 0; p < sizeof(preferred) / sizeof(preferred[0]) && !selected; p++) {
        for (int i = 0; i < n_formats; i++) {
            if ((uint32_t)formats[i].fourcc == preferred[p]) {
                selected = &formats[i];
                break;
            }
        }
    }
    if (!selected) {
        set_errorf("vaQueryImageFormats did not provide BGRA/BGRX", NULL);
        kmsvnc_vaapi_close(ctx);
        return NULL;
    }

    if (!va_ok(vaCreateImage(ctx->dpy, selected, fb_width, fb_height, &ctx->image), "vaCreateImage")) {
        kmsvnc_vaapi_close(ctx);
        return NULL;
    }
    if (!va_ok(vaMapBuffer(ctx->dpy, ctx->image.buf, &ctx->imgbuf), "vaMapBuffer")) {
        kmsvnc_vaapi_close(ctx);
        return NULL;
    }
    ctx->output_is_bgra = ((uint32_t)ctx->image.format.fourcc == VA_FOURCC_BGRA);
    ctx->src_x = src_x;
    ctx->src_y = src_y;
    ctx->width = width;
    ctx->height = height;
    ctx->fb_width = fb_width;
    ctx->fb_height = fb_height;

    fprintf(stderr, "kmsvnc vaapi init fourcc=%u pitch=%u bgra=%d src=(%u,%u) size=%ux%u fb=%ux%u\n",
        (uint32_t)ctx->image.format.fourcc,
        ctx->image.pitches[0],
        ctx->output_is_bgra,
        ctx->src_x,
        ctx->src_y,
        ctx->width,
        ctx->height,
        ctx->fb_width,
        ctx->fb_height);
    return ctx;
}

int kmsvnc_vaapi_capture(struct kmsvnc_vaapi *ctx, uint8_t *dst, size_t dst_len) {
    if (!ctx || !dst) {
        set_errorf("invalid VAAPI capture arguments", NULL);
        return 0;
    }

    size_t needed = (size_t)ctx->width * (size_t)ctx->height * 4;
    if (dst_len < needed) {
        set_errorf("destination buffer too small", NULL);
        return 0;
    }

    if (!va_ok(vaSyncSurface(ctx->dpy, ctx->surface), "vaSyncSurface")) {
        return 0;
    }
    if (!va_ok(
            vaGetImage(
                ctx->dpy,
                ctx->surface,
                0,
                0,
                ctx->fb_width,
                ctx->fb_height,
                ctx->image.image_id
            ),
            "vaGetImage")) {
        return 0;
    }

    uint8_t *src = (uint8_t *)ctx->imgbuf + ctx->image.offsets[0];
    uint32_t src_pitch = ctx->image.pitches[0];
    uint32_t row_bytes = ctx->width * 4;

    for (uint32_t y = 0; y < ctx->height; y++) {
        uint8_t *src_row = src + (size_t)(ctx->src_y + y) * src_pitch + (size_t)ctx->src_x * 4;
        uint8_t *dst_row = dst + (size_t)y * row_bytes;
        if (ctx->output_is_bgra) {
            memcpy(dst_row, src_row, row_bytes);
        } else {
            for (uint32_t x = 0; x < ctx->width; x++) {
                uint8_t *s = src_row + (size_t)x * 4;
                uint8_t *d = dst_row + (size_t)x * 4;
                d[0] = s[0];
                d[1] = s[1];
                d[2] = s[2];
                d[3] = 0xFF;
            }
        }
    }

    return 1;
}
