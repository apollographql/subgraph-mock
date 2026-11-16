//! Failing requests on a fixed timetable, e.g. to open and close a router circuit breaker.
use apollo_configuration::types::Duration as ConfigDuration;
use apollo_configuration::{ErrorCollector, configuration};
use serde::Serialize;
use std::{fmt, ops::Range, sync::OnceLock};
use tokio::{
    task::JoinHandle,
    time::{Duration, Instant, sleep_until},
};
use tracing::info;

/// When requests fail, measured from the first GraphQL request this process receives.
#[configuration(validate = validate_schedule)]
#[derive(Copy, Serialize)]
pub struct FailureScheduleConfig {
    /// How long after the first request the first failure window opens.
    #[config(default = ConfigDuration::from(Duration::ZERO))]
    pub start: ConfigDuration,
    /// How long each failure window lasts.
    #[config(required)]
    pub duration: ConfigDuration,
    /// How often a new window opens, measured from `start`. Omit for a single window.
    pub period: Option<ConfigDuration>,
    /// What a request inside a window gets.
    #[config(default = Failure::HttpStatus(503))]
    pub failure: Failure,
}

/// How a request inside a failure window fails.
#[configuration]
#[derive(Copy, Serialize)]
pub enum Failure {
    /// Respond immediately with this status (400-599) and an empty body.
    HttpStatus(u16),
    /// Respond normally, but only after this extra delay.
    Delay(ConfigDuration),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HttpStatus(status) => write!(f, "http_status {status}"),
            Self::Delay(delay) => write!(f, "delay {delay}"),
        }
    }
}

fn validate_schedule(schedule: &FailureScheduleConfig, mut errors: ErrorCollector<'_>) {
    if schedule.duration.is_zero() {
        errors
            .nest("duration")
            .report_simple("duration must be greater than zero");
    }
    if let Some(period) = schedule.period
        && *period <= *schedule.duration
    {
        errors
            .nest("period")
            .report_simple("period must be longer than duration");
    }
    if let Failure::HttpStatus(status) = schedule.failure
        && !(400..=599).contains(&status)
    {
        errors
            .nest("failure")
            .nest("http_status")
            .report_simple("http_status must be between 400 and 599");
    }
}

impl FailureScheduleConfig {
    /// The failure to apply to a request arriving `elapsed` after the clock started, or `None`
    /// outside every window.
    pub fn failure_at(&self, elapsed: Duration) -> Option<Failure> {
        self.window_index_at(elapsed).map(|_| self.failure)
    }

    /// The index of the window containing `elapsed`, or `None` outside every window. Windows
    /// include their start and exclude their end.
    fn window_index_at(&self, elapsed: Duration) -> Option<u64> {
        let since_start = elapsed.checked_sub(*self.start)?;
        let (index, into_window) = match self.period {
            None => (0, since_start),
            Some(period) => {
                let index = since_start.as_nanos() / period.as_nanos();
                let into_window = since_start.as_nanos() % period.as_nanos();
                (index as u64, Duration::from_nanos(into_window as u64))
            }
        };
        (into_window < *self.duration).then_some(index)
    }

    /// When window `index` opens and closes relative to the clock start, or `None` if the
    /// schedule has no such window.
    fn window(&self, index: u64) -> Option<Range<Duration>> {
        let offset = match self.period {
            None if index > 0 => return None,
            None => Duration::ZERO,
            Some(period) => period.checked_mul(u32::try_from(index).ok()?)?,
        };
        let start = self.start.checked_add(offset)?;
        Some(start..start.checked_add(*self.duration)?)
    }
}

/// A configured schedule and the subgraph it belongs to: `None` for the top-level schedule,
/// which covers every subgraph without its own override.
pub type NamedSchedule = (Option<String>, FailureScheduleConfig);

/// The process-wide clock every failure schedule is measured against.
///
/// The clock starts at the first GraphQL request rather than at process start, so that replicas
/// started at different times still fail in step once load reaches them.
pub struct FailureScheduleClock {
    schedules: Vec<NamedSchedule>,
    started: OnceLock<StartedClock>,
}

struct StartedClock {
    at: Instant,
    /// One task per schedule, logging each window's start and end as it happens.
    window_loggers: Vec<JoinHandle<()>>,
}

impl Drop for StartedClock {
    fn drop(&mut self) {
        self.window_loggers.iter().for_each(JoinHandle::abort);
    }
}

impl FailureScheduleClock {
    pub fn new(schedules: Vec<NamedSchedule>) -> Self {
        Self {
            schedules,
            started: OnceLock::new(),
        }
    }

    /// Time since the first GraphQL request, starting the clock if this is that request.
    ///
    /// Must be called from within a tokio runtime.
    pub fn elapsed(&self) -> Duration {
        self.started
            .get_or_init(|| self.start(Instant::now()))
            .at
            .elapsed()
    }

    fn start(&self, at: Instant) -> StartedClock {
        if !self.schedules.is_empty() {
            info!("failure schedule clock started");
        }
        let window_loggers = self
            .schedules
            .iter()
            .cloned()
            .map(|(subgraph, schedule)| tokio::spawn(log_windows(at, subgraph, schedule)))
            .collect();

        StartedClock { at, window_loggers }
    }

    /// Whether each configured schedule is inside a failure window right now. Before the clock
    /// starts, none is.
    pub fn active(&self) -> impl Iterator<Item = (Option<&str>, bool)> {
        let elapsed = self.started.get().map(|clock| clock.at.elapsed());
        self.schedules.iter().map(move |(subgraph, schedule)| {
            let active = elapsed.is_some_and(|elapsed| schedule.failure_at(elapsed).is_some());
            (subgraph.as_deref(), active)
        })
    }

    pub fn has_schedules(&self) -> bool {
        !self.schedules.is_empty()
    }
}

async fn log_windows(
    clock_start: Instant,
    subgraph: Option<String>,
    schedule: FailureScheduleConfig,
) {
    let subgraph = subgraph.as_deref();
    let failure = schedule.failure.to_string();
    for window in (0..).map_while(|index| schedule.window(index)) {
        sleep_until(clock_start + window.start).await;
        info!(subgraph, failure, "failure window started");
        sleep_until(clock_start + window.end).await;
        info!(subgraph, failure, "failure window ended");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    fn schedule(start: u64, duration: u64, period: Option<u64>) -> FailureScheduleConfig {
        FailureScheduleConfig {
            start: secs(start).into(),
            duration: secs(duration).into(),
            period: period.map(|period| secs(period).into()),
            failure: Failure::HttpStatus(503),
        }
    }

    #[test]
    fn single_window_includes_its_start_and_excludes_its_end() {
        let schedule = schedule(60, 30, None);

        assert_eq!(schedule.window_index_at(secs(0)), None);
        assert_eq!(
            schedule.window_index_at(secs(60) - Duration::from_nanos(1)),
            None
        );
        assert_eq!(schedule.window_index_at(secs(60)), Some(0));
        assert_eq!(
            schedule.window_index_at(secs(90) - Duration::from_nanos(1)),
            Some(0)
        );
        assert_eq!(schedule.window_index_at(secs(90)), None);
        assert_eq!(schedule.window_index_at(secs(150)), None);
    }

    #[test]
    fn period_repeats_the_window() {
        let schedule = schedule(60, 30, Some(90));

        assert_eq!(schedule.window_index_at(secs(89)), Some(0));
        assert_eq!(schedule.window_index_at(secs(90)), None);
        assert_eq!(schedule.window_index_at(secs(149)), None);
        assert_eq!(schedule.window_index_at(secs(150)), Some(1));
        assert_eq!(schedule.window_index_at(secs(240)), Some(2));
    }

    #[test]
    fn start_defaults_to_the_first_request() {
        let schedule = schedule(0, 10, None);

        assert_eq!(schedule.window_index_at(Duration::ZERO), Some(0));
        assert_eq!(schedule.window_index_at(secs(10)), None);
    }

    #[test]
    fn windows_list_each_opening_and_closing() {
        assert_eq!(
            schedule(60, 30, Some(90)).window(2),
            Some(secs(240)..secs(270))
        );
        assert_eq!(schedule(60, 30, None).window(0), Some(secs(60)..secs(90)));
        assert_eq!(schedule(60, 30, None).window(1), None);
    }

    #[test]
    fn failure_at_reports_the_configured_failure_inside_a_window() {
        let mut schedule = schedule(0, 10, None);
        schedule.failure = Failure::Delay(secs(2).into());

        assert!(matches!(
            schedule.failure_at(secs(5)),
            Some(Failure::Delay(delay)) if *delay == secs(2)
        ));
        assert!(schedule.failure_at(secs(10)).is_none());
    }
}
