//! Tray status model.
//!
//! `Off` when the engine is stopped, `Ready` while it advertises with no device,
//! `Connected` while a device is streaming. The engine (`engine.rs`) detects
//! connect/disconnect from UxPlay's log markers and pushes `Status` over a
//! channel; the tray maps it to the icon/tooltip.

use std::sync::atomic::AtomicBool;

/// Three states the tray icon (and tooltip) can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Off,
    Ready,
    Connected,
}

/// The engine logged a fatal start error. Set by the per-OS engine log callback
/// on the engine's own thread, consumed by the tray when the engine reports
/// `Off`: it is what separates "the run died on its own" from "the user pressed
/// Stop", and only the first of those deserves a notification.
pub static START_FAILED: AtomicBool = AtomicBool::new(false);

/// The engine started, but the adapter the user pinned was not there and it fell
/// back to listening on every interface. Set by the per-OS engine log callback,
/// read by the tray's menu builder — the receiver WORKS, so nothing else changes
/// (no notification, no icon change), but the user is not running the setup they
/// think they are and the only other record of that is a line in engine.log.
pub static PIN_IGNORED: AtomicBool = AtomicBool::new(false);

/// Clear the per-run flags. Called by each engine backend where it already resets
/// its other per-run statics, i.e. BEFORE the engine can log anything — so a flag
/// can never leak from the previous run into this one (the user fixes the adapter,
/// restarts, and the warning must be gone).
pub fn reset_run_flags() {
    START_FAILED.store(false, std::sync::atomic::Ordering::SeqCst);
    PIN_IGNORED.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Did the engine just say it is ignoring the pinned adapter?
///
/// One `LOGE` in uxplay.cpp, and the only occurrence of this wording anywhere in
/// the fork (the other "adapter" hits are comments in `lib/netutils.*`). It is
/// deliberately NOT in [`is_fatal_start_line`]: the engine carries on afterwards.
pub fn is_pin_ignored_line(msg: &str) -> bool {
    msg.contains("no up local adapter has address")
}

/// Does this engine log line mean the run is over before it began?
///
/// The C ABI cannot tell us: `airplay_core_start` returns the moment the worker
/// thread is spawned, long before that worker reaches mDNS registration, so a
/// receiver that never came up looks exactly like one that did. Teaching the ABI
/// to report it would mean an exe and a `uxplay-core` that must ship as a matched
/// pair; the log stream already carries the answer and needs no such contract.
///
/// Every substring below was grepped across uxplay.cpp, `lib/` and `renderers/`
/// and appears ONLY on a path that returns a failure out of
/// `airplay_run_blocking()`. That bar is deliberately high: the host hook runs
/// BEFORE the engine's log-level filter, so this sees per-packet DEBUG chatter
/// too, and a false alarm mid-stream is worse than a missed failure.
///
/// Notably NOT matched: `"no up local adapter has address ... -- listening on all
/// adapters"`. It is a `LOGE`, which is why the callback's level argument is no
/// use as a filter — but it is the documented `-bind` fallback and the engine
/// runs on afterwards.
pub fn is_fatal_start_line(msg: &str) -> bool {
    // Only at the START of the line: the mirror's DEBUG chatter ("This packet
    // indicates video stream is stopping", "stopping RAOP mirror") carries the
    // word mid-sentence during perfectly healthy playback.
    msg.starts_with("stopping")
        // start_dnssd(): the pinned-adapter-vanished case this whole feature
        // exists for. Both are distinct from the sibling LOGI "dnssd_register_raop
        // failed: ignoring because Bluetooth LE ..." — which is NOT a failure.
        || msg.contains("failed with error code")
        || msg.contains("DNSServiceRegister call returned")
        || msg.contains("Could not initialize dnssd library!")
        // start_raop_server(): raop_init/raop_init2 refused (ports held by
        // another instance, bad key file).
        || msg.contains("Error initializing raop")
        // -rc is handled BEFORE parse_arguments, so its two failures never get
        // the "stopping:" wrapper the later ones have — a typo'd -rc in Settings
        // -> Advanced returned -1 with the tray still green. Anchored at the
        // start of the line: these words are user-supplied-data-adjacent (the
        // filename is echoed), and every metadata line the engine logs begins
        // with its own label ("Title: ", "Artist: ", uxplay.cpp process_metadata).
        || msg.starts_with("option -rc requires a filename")
        || msg.starts_with("startup file ")
}

#[cfg(test)]
mod tests {
    use super::is_fatal_start_line;

    #[test]
    fn fatal_start_lines_are_recognised() {
        for line in [
            "stopping",                                             // gstreamer_init failed
            "stopping: bad option in startup file /home/u/.uxplayrc",
            "stopping: engine started with an option it does not accept (see the message above; check Settings -> Advanced)",
            "dnssd_register_raop failed with error code -65537\nmDNS Error codes are in range",
            "dnssd_register_airplay failed with error code -65537\nmDNS Error codes are in range",
            "No DNS-SD Server found (DNSServiceRegister call returned kDNSServiceErr_Unknown)",
            "DNSServiceRegister call returned kDNSServiceErr_NameConflict",
            "Could not initialize dnssd library!: error -65537",
            "Error initializing raop!",
            // -rc: rejected before parse_arguments, hence no "stopping:" prefix.
            "startup file /home/u/gone.rc specified by option -rc was not found",
            "option -rc requires a filename  (-rc <filename>)",
        ] {
            assert!(is_fatal_start_line(line), "missed: {line}");
        }
    }

    #[test]
    fn healthy_lines_never_raise_a_false_alarm() {
        for line in [
            // The whole reason the substrings are this long. Every one of these
            // reaches the callback during a normal session.
            "This packet indicates video stream is stopping",
            "New AirPlay connection: stopping RAOP mirror",
            "raop_ntp stopping time thread",
            // A LOGE that is a documented fallback, not a failed start.
            "no up local adapter has address 192.168.1.50 -- listening on all adapters",
            // Same words as the fatal dnssd lines, opposite meaning.
            "dnssd_register_raop failed: ignoring because Bluetooth LE service discovery may be available",
            "dnssd_register_airplay failed: ignoring because Bluetooth LE service discovery may be available",
            "Begin streaming to iPhone",
            "Open connections: 0",
            "bound to 192.168.1.50 (mDNS interface index 7); IPv6 and loopback listeners are off",
            // Track metadata is logged verbatim, so a title CAN carry our words —
            // it just cannot carry them at the start of the line (every metadata
            // line is prefixed with its DMAP label).
            "Title: startup file was not found",
            "Album: option -rc requires a filename",
        ] {
            assert!(!is_fatal_start_line(line), "false alarm: {line}");
        }
    }

    #[test]
    fn the_pin_fallback_is_a_warning_not_a_failure() {
        let fallback = "no up local adapter has address 192.168.1.50 -- listening on all adapters";
        assert!(super::is_pin_ignored_line(fallback));
        // The engine keeps running on this path, so it must never reach the
        // failed-start machinery (icon Off, teardown, notification).
        assert!(!is_fatal_start_line(fallback));
        // The success twin, and the failure that is NOT this one.
        for line in [
            "bound to 192.168.1.50 (mDNS interface index 7); IPv6 and loopback listeners are off",
            "dnssd_register_raop failed with error code -65537",
            "Begin streaming to iPhone",
        ] {
            assert!(!super::is_pin_ignored_line(line), "false alarm: {line}");
        }
    }
}

