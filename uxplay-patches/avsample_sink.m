// avsample_sink.m — custom macOS video sink (Plan B).
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
    // CFBridgingRetain'd AVSampleBufferDisplayLayer. _Atomic because create()
    // publishes it from the main queue while the GStreamer streaming thread is
    // already calling enqueue().
    _Atomic(void *) layer;
} AVLayerSink;

/// Create the display layer and host it in `nsview` (AppKit work on the main thread).
///
/// MUST NOT dispatch_sync to the main queue — same hazard as destroy() below: this
/// runs on the engine worker, and a host that quits mid-connect blocks its main
/// thread inside airplay_core_stop() joining that worker, so a sync hop would
/// deadlock with no window and no tray. So: inline if already on main, else
/// fire-and-forget. The caller only needs the AVLayerSink handle (it is appsink
/// callback userdata); enqueue() drops the first frames until `layer` is published,
/// which is a few ms at stream start and invisible.
void *avlayer_sink_create(void *nsview_ptr) {
    if (!nsview_ptr) return NULL;
    AVLayerSink *s = (AVLayerSink *)calloc(1, sizeof(AVLayerSink));
    if (!s) return NULL;
    NSView *view = (__bridge NSView *)nsview_ptr;
    void (^build)(void) = ^{
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
    };
    if ([NSThread isMainThread]) {
        build();
    } else {
        dispatch_async(dispatch_get_main_queue(), build);
    }
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
    CVReturn rc;
    // The caller is a GStreamer streaming thread, which has no autorelease pool of
    // its own: an autoreleased literal there is held until the thread dies. Only
    // this dictionary is autorelease-prone, so scope a pool around just it.
    @autoreleasepool {
        NSDictionary *attrs = @{ (id)kCVPixelBufferIOSurfacePropertiesKey : @{} };
        rc = CVPixelBufferCreate(kCFAllocatorDefault, width, height,
                                 kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                                 (__bridge CFDictionaryRef)attrs, &pb);
    }
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
/// captures `s`, whose ownership passes to the block (single CFBridgingRelease
/// transfers the layer exactly once).
///   * On restart the async cleanup drains FIFO before the next create() bind (the
///     serial main queue + distinct CALayer instances keep the layer tree correct).
///   * On app quit the process exit()s before the main queue drains again, so the
///     block is simply abandoned — harmless, teardown at exit is moot.
///
/// Reading `s->layer` and freeing `s` happen INSIDE the block, not before it:
/// create() may still have its own block queued ahead of ours, so the layer only
/// exists once that has run, and an earlier free() would let it write freed memory.
/// The serial main queue gives us that ordering because create() and destroy() are
/// always called from the same thread (the engine worker, or main in standalone
/// uxplay) — the pairing that video_renderer.c enforces.
void avlayer_sink_destroy(void *sink_) {
    AVLayerSink *s = (AVLayerSink *)sink_;
    if (!s) return;
    void (^cleanup)(void) = ^{
        void *layer_ref = s->layer;
        s->layer = NULL;
        if (layer_ref) {
            AVSampleBufferDisplayLayer *layer = (AVSampleBufferDisplayLayer *)CFBridgingRelease(layer_ref);
            [layer flush];
            [layer removeFromSuperlayer];
        }
        free(s);
    };
    if ([NSThread isMainThread]) {
        cleanup();
    } else {
        dispatch_async(dispatch_get_main_queue(), cleanup);
    }
}
