use crate::types::{CloseReason, OpenDurationClass, WindowObservables};
use crate::config::MIN_ARB_TICKS;

/// Classify a closing window on both dimensions using stored observables.
/// Returns (OpenDurationClass, Option<CloseReason>).
/// CloseReason is None for single_tick windows (not scored).
pub fn classify(obs: &WindowObservables) -> (OpenDurationClass, Option<CloseReason>) {
    let open_class = if obs.tick_count < MIN_ARB_TICKS {
        OpenDurationClass::SingleTick
    } else {
        OpenDurationClass::MultiTick
    };

    if open_class == OpenDurationClass::SingleTick {
        return (open_class, None);
    }

    let close_reason = if obs.trade_event_fired {
        if obs.volume_change_ticks > 1 {
            CloseReason::VolumeSpikeGradual
        } else {
            CloseReason::VolumeSpikeInstant
        }
    } else if obs.price_shifted {
        CloseReason::PriceDrift
    } else {
        CloseReason::OrderVanished
    };

    (open_class, Some(close_reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(tick_count: u32, trade: bool, volume_ticks: u32, price_shifted: bool) -> WindowObservables {
        WindowObservables {
            tick_count,
            trade_event_fired: trade,
            volume_change_ticks: volume_ticks,
            price_shifted,
        }
    }

    #[test]
    fn single_tick_is_noise() {
        let (class, reason) = classify(&obs(1, true, 3, true));
        assert_eq!(class, OpenDurationClass::SingleTick);
        assert!(reason.is_none());
    }

    #[test]
    fn multi_tick_gradual_spike() {
        let (class, reason) = classify(&obs(3, true, 2, false));
        assert_eq!(class, OpenDurationClass::MultiTick);
        assert_eq!(reason, Some(CloseReason::VolumeSpikeGradual));
    }

    #[test]
    fn multi_tick_instant_spike() {
        let (class, reason) = classify(&obs(3, true, 1, false));
        assert_eq!(class, OpenDurationClass::MultiTick);
        assert_eq!(reason, Some(CloseReason::VolumeSpikeInstant));
    }

    #[test]
    fn multi_tick_price_drift() {
        let (class, reason) = classify(&obs(4, false, 0, true));
        assert_eq!(class, OpenDurationClass::MultiTick);
        assert_eq!(reason, Some(CloseReason::PriceDrift));
    }

    #[test]
    fn multi_tick_order_vanished() {
        let (class, reason) = classify(&obs(2, false, 0, false));
        assert_eq!(class, OpenDurationClass::MultiTick);
        assert_eq!(reason, Some(CloseReason::OrderVanished));
    }
}
