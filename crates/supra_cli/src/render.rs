#![deny(missing_docs)]

/// Fold `deliveries` into `frame` and render the status line.
///
/// `missed_before` rides each delivery: the bus reports the gap on the
/// delivery the reader actually wanted, so a slow consumer's loss is
/// visible in the same line it draws.
///
/// Pure rendering: the caller drains the bus and hands the deliveries
/// here. No terminal I/O - stdout writes stay with the caller, so tests
/// assert on strings.
pub(crate) fn render_drain(
    frame: &mut supra_tui::FrameState,
    deliveries: &[supra_eventbus::Delivery],
    mode: &str,
    model: &str,
    theme: &supra_theme::Theme,
    cols: usize,
) -> Vec<String> {
    for delivery in deliveries {
        frame.apply(&delivery.event, delivery.missed_before);
    }
    let cost = supra_tui::Cost { mills: frame.spend_mills(), estimated: frame.completions == 0 };
    let line = supra_tui::StatusLine::live(
        mode,
        0,
        0,
        cost,
        frame.cache_broken,
        model,
        false,
        frame.missed_events,
        0,
    );
    vec![line.render(cols, theme)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> supra_theme::Theme {
        supra_theme::Theme::default_dark()
    }

    #[test]
    fn a_drain_folds_usage_into_a_spend_segment() {
        use supra_eventbus::TopicSet;
        let bus = supra_eventbus::Bus::new();
        let watcher = bus.subscribe(TopicSet::all());
        bus.publish(supra_types::Event::UsageReported {
            agent: supra_types::AgentId::generate(),
            cached_read: 900,
            cached_write: 0,
            uncached: 100,
            output: 50,
            cost: supra_types::MicroUsd::from_micros(1_400),
        });
        let frame_deliveries = watcher.drain();
        let mut frame = supra_tui::FrameState::default();
        let lines = render_drain(&mut frame, &frame_deliveries, "auto", "sonnet", &theme(), 120);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("+$0.002"), "2000 micros rounds up: {}", lines[0]);
        assert!(lines[0].contains("mode auto"), "{}", lines[0]);
        assert_eq!(frame.completions, 1);
    }

    #[test]
    fn a_cache_break_and_a_gap_surface_in_the_same_line() {
        use supra_eventbus::TopicSet;
        let bus = supra_eventbus::Bus::new();
        let watcher = bus.subscribe(TopicSet::all());
        bus.publish(supra_types::Event::CacheBreak {
            cause: supra_types::CacheBreakCause::VolatileDataInPrefix,
            detail: "probe".to_owned(),
        });
        bus.publish(supra_types::Event::StreamDelta { chars: 10 });
        let frame_deliveries = watcher.drain();
        let mut frame = supra_tui::FrameState::default();
        let lines = render_drain(&mut frame, &frame_deliveries, "yolo", "opus", &theme(), 120);
        assert!(lines[0].contains("cache broke"), "{}", lines[0]);
        assert!(lines[0].contains("mode yolo"), "{}", lines[0]);
    }
}
