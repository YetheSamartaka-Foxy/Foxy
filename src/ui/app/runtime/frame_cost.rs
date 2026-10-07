use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Parts of `Foxy::update` timed on every frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameSection {
    /// Channel polls, background results and queues before any drawing.
    Polls,
    /// The window chrome: title bar, navigation, footer.
    MainView,
    /// The current view's content.
    Content,
    /// Toasts, notices and prompts drawn over the view.
    Overlays,
}

const SECTIONS: usize = 4;

/// Running totals of the UI thread's frames since launch; the difference
/// between two snapshots is what a span of frames cost.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FrameCostTotals {
    frames: u64,
    /// Frames each repaint request site caused, keyed by file and line.
    causes: HashMap<(&'static str, u32), u64>,
    /// UI thread CPU at the start of the latest frame, which also covers the
    /// tessellation and painting eframe does between two `update` calls.
    ui_thread_cpu: Duration,
    sections: [Duration; SECTIONS],
}

impl FrameCostTotals {
    pub(crate) fn begin_frame(
        &mut self,
        ui_thread_cpu: Option<Duration>,
        causes: impl IntoIterator<Item = (&'static str, u32)>,
    ) {
        self.frames += 1;
        for cause in causes {
            *self.causes.entry(cause).or_default() += 1;
        }
        if let Some(cpu) = ui_thread_cpu {
            self.ui_thread_cpu = cpu;
        }
    }

    pub(crate) fn add(&mut self, section: FrameSection, elapsed: Duration) {
        self.sections[section as usize] += elapsed;
    }

    fn since(&self, earlier: &FrameCostTotals) -> FrameCostTotals {
        let mut sections = [Duration::ZERO; SECTIONS];
        for (index, slot) in sections.iter_mut().enumerate() {
            *slot = self.sections[index].saturating_sub(earlier.sections[index]);
        }
        let causes = self
            .causes
            .iter()
            .map(|(cause, count)| {
                let before = earlier.causes.get(cause).copied().unwrap_or(0);
                (*cause, count.saturating_sub(before))
            })
            .filter(|(_, count)| *count > 0)
            .collect();
        FrameCostTotals {
            frames: self.frames.saturating_sub(earlier.frames),
            causes,
            ui_thread_cpu: self.ui_thread_cpu.saturating_sub(earlier.ui_thread_cpu),
            sections,
        }
    }
}

/// Times one section of a frame and adds it to the totals when dropped.
pub(crate) struct SectionTimer {
    section: FrameSection,
    started: Instant,
}

impl SectionTimer {
    pub(crate) fn start(section: FrameSection) -> Self {
        Self {
            section,
            started: Instant::now(),
        }
    }

    pub(crate) fn stop(self, totals: &mut FrameCostTotals) {
        totals.add(self.section, self.started.elapsed());
    }
}

/// Frame count, rate, UI-thread CPU, `update` sections and the five repaint
/// request sites that caused the most frames, between `start` and `end`,
/// `wall` apart.
pub(crate) fn describe(start: &FrameCostTotals, end: &FrameCostTotals, wall: Duration) -> String {
    let span = end.since(start);
    let frames = span.frames.max(1) as f64;
    let secs = |duration: Duration| duration.as_secs_f64();
    let per_frame_ms = |duration: Duration| duration.as_secs_f64() * 1000.0 / frames;
    let update: Duration = span.sections.iter().sum();
    let mut causes: Vec<_> = span.causes.iter().collect();
    causes.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let causes: Vec<String> = causes
        .iter()
        .take(5)
        .map(|((file, line), count)| format!("{file}:{line}x{count}"))
        .collect();
    format!(
        "frames={} fps={:.1} ui_cpu={:.2}s per_frame_cpu={:.2}ms update={:.2}s per_frame_update={:.2}ms (polls={:.2}ms main={:.2}ms content={:.2}ms overlays={:.2}ms) causes=[{}]",
        span.frames,
        span.frames as f64 / wall.as_secs_f64().max(f64::EPSILON),
        secs(span.ui_thread_cpu),
        per_frame_ms(span.ui_thread_cpu),
        secs(update),
        per_frame_ms(update),
        per_frame_ms(span.sections[FrameSection::Polls as usize]),
        per_frame_ms(span.sections[FrameSection::MainView as usize]),
        per_frame_ms(span.sections[FrameSection::Content as usize]),
        per_frame_ms(span.sections[FrameSection::Overlays as usize]),
        causes.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_the_frames_between_two_snapshots() {
        let mut totals = FrameCostTotals::default();
        totals.begin_frame(Some(Duration::from_millis(500)), [("a.rs", 1)]);
        totals.add(FrameSection::Content, Duration::from_millis(3));
        let start = totals.clone();
        for frame in 1..=4 {
            totals.begin_frame(
                Some(Duration::from_millis(500 + frame * 10)),
                [("a.rs", 1), ("b.rs", 7)]
                    .into_iter()
                    .take(frame as usize % 2 + 1),
            );
            totals.add(FrameSection::Polls, Duration::from_millis(1));
            totals.add(FrameSection::Content, Duration::from_millis(5));
        }
        totals.begin_frame(None, []);
        assert_eq!(
            describe(&start, &totals, Duration::from_secs(1)),
            "frames=5 fps=5.0 ui_cpu=0.04s per_frame_cpu=8.00ms update=0.02s per_frame_update=4.80ms (polls=0.80ms main=0.00ms content=4.00ms overlays=0.00ms) causes=[a.rs:1x4, b.rs:7x2]"
        );
    }
}
