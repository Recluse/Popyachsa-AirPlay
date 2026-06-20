/*
 * UxPlay - An open-source AirPlay mirroring server
 * Copyright (C) 2021-24 F. Duncanh
 * uxplay-core embeddable-library additions
 * Copyright (C) 2026 Recluse
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software Foundation,
 * Inc., 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301  USA
 */

// avsample_sink.m — custom macOS video sink.
//
// Bypasses GStreamer's buggy applemedia sinks (avsamplebufferlayersink UAF on
// caps-change, osxvideosink teardown deadlock): we pull decoded NV12 frames from
// an `appsink` (video_renderer.c) and feed them to OUR OWN
// AVSampleBufferDisplayLayer, hosted in the app's NSView. Because we own the
// layer and the enqueue path, there is no framework UAF and no main-thread
// teardown deadlock, and the layer scales cleanly on resize/rotation.
//
// Compiled as Objective-C (ARC) into the `renderers` lib; exposed to the C TU
// video_renderer.c via the small C ABI below.

#import <AVFoundation/AVFoundation.h>
#import <AppKit/AppKit.h>
#import <CoreVideo/CoreVideo.h>
#import <CoreMedia/CoreMedia.h>

// --- C ABI (called from video_renderer.c) -------------------------------------
void *avlayer_sink_create(void *nsview_ptr);
void  avlayer_sink_enqueue_nv12(void *sink, const unsigned char *y, unsigned long y_stride,
                                const unsigned char *uv, unsigned long uv_stride,
                                int width, int height);
void  avlayer_sink_destroy(void *sink);

typedef struct AVLayerSink {
    void *layer; // CFBridgingRetain'd AVSampleBufferDisplayLayer
} AVLayerSink;

/// Create the display layer and host it in `nsview` (on the main thread).
void *avlayer_sink_create(void *nsview_ptr) {
    if (!nsview_ptr) return NULL;
    AVLayerSink *s = (AVLayerSink *)calloc(1, sizeof(AVLayerSink));
    if (!s) return NULL;
    NSView *view = (__bridge NSView *)nsview_ptr;
    dispatch_sync(dispatch_get_main_queue(), ^{
        AVSampleBufferDisplayLayer *layer = [[AVSampleBufferDisplayLayer alloc] init];
        layer.videoGravity = AVLayerVideoGravityResizeAspect; // honest aspect, letterbox
        view.wantsLayer = YES;
        CALayer *backing = view.layer;
        backing.backgroundColor = CGColorGetConstantColor(kCGColorBlack);
        layer.frame = view.bounds;
        layer.backgroundColor = CGColorGetConstantColor(kCGColorBlack);
        // kCALayerWidthSizable | kCALayerHeightSizable -> follows the view on resize
        layer.autoresizingMask = kCALayerWidthSizable | kCALayerHeightSizable;
        [backing addSublayer:layer];
        s->layer = (void *)CFBridgingRetain(layer);
    });
    return s;
}

/// Wrap an NV12 frame in a CVPixelBuffer + CMSampleBuffer and enqueue it. Safe to
/// call from the GStreamer streaming thread (AVSampleBufferDisplayLayer enqueue is
/// thread-safe). Frames are tagged display-immediately (lowest latency).
void avlayer_sink_enqueue_nv12(void *sink_, const unsigned char *y, unsigned long y_stride,
                               const unsigned char *uv, unsigned long uv_stride,
                               int width, int height) {
    AVLayerSink *s = (AVLayerSink *)sink_;
    if (!s || !s->layer || !y || !uv || width <= 0 || height <= 0) return;
    AVSampleBufferDisplayLayer *layer = (__bridge AVSampleBufferDisplayLayer *)s->layer;

    // If the layer failed (e.g. went to background), flush so it accepts frames again.
    if (layer.status == AVQueuedSampleBufferRenderingStatusFailed) {
        [layer flush];
    }

    CVPixelBufferRef pb = NULL;
    NSDictionary *attrs = @{ (id)kCVPixelBufferIOSurfacePropertiesKey : @{} };
    CVReturn rc = CVPixelBufferCreate(kCFAllocatorDefault, width, height,
                                      kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                                      (__bridge CFDictionaryRef)attrs, &pb);
    if (rc != kCVReturnSuccess || !pb) return;

    if (CVPixelBufferLockBaseAddress(pb, 0) != kCVReturnSuccess) {
        CVPixelBufferRelease(pb); // never unlock a lock that did not succeed
        return;
    }
    unsigned char *dy = (unsigned char *)CVPixelBufferGetBaseAddressOfPlane(pb, 0);
    unsigned long dys = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
    for (int row = 0; row < height; row++) {
        memcpy(dy + (unsigned long)row * dys, y + (unsigned long)row * y_stride, (size_t)width);
    }
    unsigned char *duv = (unsigned char *)CVPixelBufferGetBaseAddressOfPlane(pb, 1);
    unsigned long duvs = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);
    for (int row = 0; row < height / 2; row++) {
        memcpy(duv + (unsigned long)row * duvs, uv + (unsigned long)row * uv_stride, (size_t)width);
    }
    CVPixelBufferUnlockBaseAddress(pb, 0);

    CMVideoFormatDescriptionRef fmt = NULL;
    if (CMVideoFormatDescriptionCreateForImageBuffer(kCFAllocatorDefault, pb, &fmt) != noErr || !fmt) {
        CVPixelBufferRelease(pb);
        return;
    }

    CMSampleTimingInfo timing = { kCMTimeInvalid, kCMTimeInvalid, kCMTimeInvalid };
    CMSampleBufferRef sb = NULL;
    OSStatus st = CMSampleBufferCreateReadyWithImageBuffer(kCFAllocatorDefault, pb, fmt, &timing, &sb);
    if (st == noErr && sb) {
        CFArrayRef atts = CMSampleBufferGetSampleAttachmentsArray(sb, true);
        if (atts && CFArrayGetCount(atts) > 0) {
            CFMutableDictionaryRef d = (CFMutableDictionaryRef)CFArrayGetValueAtIndex(atts, 0);
            CFDictionarySetValue(d, kCMSampleAttachmentKey_DisplayImmediately, kCFBooleanTrue);
        }
        [layer enqueueSampleBuffer:sb];
        CFRelease(sb);
    }
    CFRelease(fmt);
    CVPixelBufferRelease(pb);
}

/// Detach + release the layer (AppKit work must run on the main thread).
///
/// MUST NOT dispatch_sync to the main queue: teardown runs on the engine worker
/// while the main thread is blocked inside airplay_core_stop() joining that very
/// worker (X-button restart) — a sync hop would deadlock. So we run inline if we
/// are already on main, else fire-and-forget via dispatch_async. The block only
/// captures `layer_ref` (already detached from `s`), so freeing `s` immediately is
/// safe (single CFBridgingRelease transfers ownership exactly once).
///   * On restart the async cleanup drains FIFO before the next create() bind (the
///     serial main queue + distinct CALayer instances keep the layer tree correct).
///   * On app quit the process exit()s before the main queue drains again, so the
///     block is simply abandoned — harmless, teardown at exit is moot.
void avlayer_sink_destroy(void *sink_) {
    AVLayerSink *s = (AVLayerSink *)sink_;
    if (!s) return;
    void *layer_ref = s->layer;
    s->layer = NULL;
    if (layer_ref) {
        void (^cleanup)(void) = ^{
            AVSampleBufferDisplayLayer *layer = (AVSampleBufferDisplayLayer *)CFBridgingRelease(layer_ref);
            [layer flush];
            [layer removeFromSuperlayer];
        };
        if ([NSThread isMainThread]) {
            cleanup();
        } else {
            dispatch_async(dispatch_get_main_queue(), cleanup);
        }
    }
    free(s);
}
