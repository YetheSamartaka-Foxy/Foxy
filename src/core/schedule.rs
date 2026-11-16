//! Calendar math shared by the desktop repository scheduler and server maintenance jobs.
//!
//! A local time skipped by a daylight-saving gap resolves to the first valid instant after it;
//! a repeated local time resolves to its earlier instant, so it fires once.

use chrono::{DateTime, Datelike, Local, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// `OnceAt` stores an absolute instant; `Recurring` stores a time of day and selected weekdays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobSchedule {
    /// Fire once at an absolute time, stored as Unix epoch milliseconds.
    OnceAt { unix_ms: u64 },
    /// `minutes_of_day` is `0..1440`; weekday bit `i` means `i` days after Monday.
    Recurring { minutes_of_day: u16, weekdays: u8 },
}

/// The civil time in which a recurring schedule is interpreted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleClock {
    #[default]
    Local,
    Utc,
}

pub fn unix_ms_to_local(ms: u64) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(ms as i64).single()
}

pub fn local_to_unix_ms(dt: DateTime<Local>) -> u64 {
    dt.timestamp_millis().max(0) as u64
}

fn recurring_fire<Tz: TimeZone>(
    minutes_of_day: u16,
    weekdays: u8,
    anchor: DateTime<Tz>,
    forward: bool,
) -> Option<DateTime<Tz>> {
    if weekdays == 0 {
        return None;
    }
    let minutes_of_day = minutes_of_day.min(24 * 60 - 1);
    let time = NaiveTime::from_hms_opt(
        u32::from(minutes_of_day / 60),
        u32::from(minutes_of_day % 60),
        0,
    )?;
    let zone = anchor.timezone();
    for offset in 0..8i64 {
        let date = if forward {
            anchor.date_naive() + chrono::Duration::days(offset)
        } else {
            anchor.date_naive() - chrono::Duration::days(offset)
        };
        if weekdays & (1u8 << date.weekday().num_days_from_monday()) == 0 {
            continue;
        }
        let Some(candidate) =
            first_valid_local_instant(date.and_time(time), |local| zone.from_local_datetime(local))
        else {
            continue;
        };
        if forward {
            if candidate >= anchor {
                return Some(candidate);
            }
        } else if candidate <= anchor {
            return Some(candidate);
        }
    }
    None
}

pub fn first_valid_local_instant<Tz, F>(
    requested: NaiveDateTime,
    mut resolve: F,
) -> Option<DateTime<Tz>>
where
    Tz: TimeZone,
    F: FnMut(&NaiveDateTime) -> LocalResult<DateTime<Tz>>,
{
    for minute in 0..=24 * 60 {
        let local = requested + chrono::Duration::minutes(minute);
        match resolve(&local) {
            LocalResult::Single(value) => return Some(value),
            LocalResult::Ambiguous(first, second) => {
                return Some(if first.timestamp() <= second.timestamp() {
                    first
                } else {
                    second
                });
            }
            LocalResult::None => {}
        }
    }
    None
}

fn instant_ms<Tz: TimeZone>(value: DateTime<Tz>) -> u64 {
    value.timestamp_millis().max(0) as u64
}

impl JobSchedule {
    /// Return the next scheduled instant at or after `from`, or `None` when a one-time job has passed.
    pub fn next_fire_at(&self, from: DateTime<Local>) -> Option<DateTime<Local>> {
        match self {
            JobSchedule::OnceAt { unix_ms } => {
                let fire = unix_ms_to_local(*unix_ms)?;
                (fire >= from).then_some(fire)
            }
            JobSchedule::Recurring {
                minutes_of_day,
                weekdays,
            } => recurring_fire(*minutes_of_day, *weekdays, from, true),
        }
    }

    /// Return the latest scheduled instant at or before `now`, or `None` if it has not fired.
    pub fn most_recent_fire_at_or_before(&self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        match self {
            JobSchedule::OnceAt { unix_ms } => {
                let fire = unix_ms_to_local(*unix_ms)?;
                (fire <= now).then_some(fire)
            }
            JobSchedule::Recurring {
                minutes_of_day,
                weekdays,
            } => recurring_fire(*minutes_of_day, *weekdays, now, false),
        }
    }

    /// Next instant at or after `from_ms` in Unix milliseconds, interpreting recurrence in `clock`.
    pub fn next_fire_ms(&self, from_ms: u64, clock: ScheduleClock) -> Option<u64> {
        match self {
            JobSchedule::OnceAt { unix_ms } => (*unix_ms >= from_ms).then_some(*unix_ms),
            JobSchedule::Recurring {
                minutes_of_day,
                weekdays,
            } => match clock {
                ScheduleClock::Local => recurring_fire(
                    *minutes_of_day,
                    *weekdays,
                    Local.timestamp_millis_opt(from_ms as i64).single()?,
                    true,
                )
                .map(instant_ms),
                ScheduleClock::Utc => recurring_fire(
                    *minutes_of_day,
                    *weekdays,
                    Utc.timestamp_millis_opt(from_ms as i64).single()?,
                    true,
                )
                .map(instant_ms),
            },
        }
    }

    /// Latest instant at or before `now_ms` in Unix milliseconds, interpreting recurrence in `clock`.
    pub fn most_recent_fire_ms(&self, now_ms: u64, clock: ScheduleClock) -> Option<u64> {
        match self {
            JobSchedule::OnceAt { unix_ms } => (*unix_ms <= now_ms).then_some(*unix_ms),
            JobSchedule::Recurring {
                minutes_of_day,
                weekdays,
            } => match clock {
                ScheduleClock::Local => recurring_fire(
                    *minutes_of_day,
                    *weekdays,
                    Local.timestamp_millis_opt(now_ms as i64).single()?,
                    false,
                )
                .map(instant_ms),
                ScheduleClock::Utc => recurring_fire(
                    *minutes_of_day,
                    *weekdays,
                    Utc.timestamp_millis_opt(now_ms as i64).single()?,
                    false,
                )
                .map(instant_ms),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> u64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .unwrap()
            .timestamp_millis() as u64
    }

    #[test]
    fn utc_recurrence_finds_next_and_previous_selected_weekday() {
        let schedule = JobSchedule::Recurring {
            minutes_of_day: 4 * 60 + 30,
            weekdays: 0b0000_0101,
        };
        let monday_noon = utc_ms(2026, 11, 16, 12, 0);
        assert_eq!(
            schedule.next_fire_ms(monday_noon, ScheduleClock::Utc),
            Some(utc_ms(2026, 11, 18, 4, 30))
        );
        assert_eq!(
            schedule.most_recent_fire_ms(monday_noon, ScheduleClock::Utc),
            Some(utc_ms(2026, 11, 16, 4, 30))
        );
        let exact = utc_ms(2026, 11, 18, 4, 30);
        assert_eq!(
            schedule.next_fire_ms(exact, ScheduleClock::Utc),
            Some(exact)
        );
        assert_eq!(
            schedule.most_recent_fire_ms(exact, ScheduleClock::Utc),
            Some(exact)
        );
        let none = JobSchedule::Recurring {
            minutes_of_day: 0,
            weekdays: 0,
        };
        assert_eq!(none.next_fire_ms(monday_noon, ScheduleClock::Utc), None);
    }

    #[test]
    fn one_time_schedules_ignore_the_clock() {
        let at = utc_ms(2026, 11, 20, 8, 0);
        let once = JobSchedule::OnceAt { unix_ms: at };
        for clock in [ScheduleClock::Local, ScheduleClock::Utc] {
            assert_eq!(once.next_fire_ms(at - 1, clock), Some(at));
            assert_eq!(once.next_fire_ms(at + 1, clock), None);
            assert_eq!(once.most_recent_fire_ms(at + 1, clock), Some(at));
            assert_eq!(once.most_recent_fire_ms(at - 1, clock), None);
        }
    }

    #[test]
    fn local_and_utc_millisecond_paths_agree_with_the_local_datetime_path() {
        let schedule = JobSchedule::Recurring {
            minutes_of_day: 23 * 60 + 15,
            weekdays: 0b0111_1111,
        };
        let now = Local::now();
        let expected = schedule.next_fire_at(now).map(local_to_unix_ms);
        assert_eq!(
            schedule.next_fire_ms(local_to_unix_ms(now), ScheduleClock::Local),
            expected
        );
    }
}
