//! 本地日历规则只解析日期与时间，比较和排序使用绝对时间。

use assistant_protocol::{ScheduleRule, ScheduleWeekday};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc, Weekday};

pub(super) enum CalendarRule {
    Once(NaiveDateTime),
    Daily(NaiveTime),
    Weekly {
        time: NaiveTime,
        weekdays: Vec<ScheduleWeekday>,
    },
}

impl CalendarRule {
    pub(super) fn parse(rule: &ScheduleRule) -> Result<Self, &'static str> {
        match rule {
            ScheduleRule::Once { date, time } => {
                let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")
                    .ok()
                    .filter(|value| {
                        date.len() == 10 && value.format("%Y-%m-%d").to_string() == *date
                    })
                    .ok_or("schedule date must be a valid YYYY-MM-DD date")?;
                Ok(Self::Once(date.and_time(parse_time(time)?)))
            }
            ScheduleRule::Daily { time } => Ok(Self::Daily(parse_time(time)?)),
            ScheduleRule::Weekly { time, weekdays } => {
                if !(1..=7).contains(&weekdays.len()) {
                    return Err("weekly schedule requires distinct weekdays");
                }
                let mut unique = weekdays.clone();
                unique.sort_unstable();
                unique.dedup();
                if unique.len() != weekdays.len() {
                    return Err("weekly schedule requires distinct weekdays");
                }
                Ok(Self::Weekly {
                    time: parse_time(time)?,
                    weekdays: unique,
                })
            }
        }
    }

    pub(super) fn next_after<Tz: TimeZone>(
        &self,
        timezone: &Tz,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Tz>> {
        let occurrence = |local: NaiveDateTime| {
            // 时钟回拨的重叠分钟只取第一次；即使已过第一次也不换到第二次。
            // 春季跳时没有对应瞬间，周期计划继续寻找下一个匹配日。
            timezone
                .from_local_datetime(&local)
                .earliest()
                .filter(|time| time.with_timezone(&Utc) > now)
        };
        let (time, weekdays) = match self {
            Self::Once(local) => return occurrence(*local),
            Self::Daily(time) => (*time, None),
            Self::Weekly { time, weekdays } => (*time, Some(weekdays)),
        };
        let mut date = now.with_timezone(timezone).date_naive();
        // 查到下两周的同一天，覆盖本周已过、下周一次夏令时缺口的情形。
        for _ in 0..=14 {
            if weekdays.is_none_or(|days| days.contains(&schedule_weekday(date.weekday())))
                && let Some(next) = occurrence(date.and_time(time))
            {
                return Some(next);
            }
            date = date.succ_opt()?;
        }
        None
    }
}

fn parse_time(raw: &str) -> Result<NaiveTime, &'static str> {
    NaiveTime::parse_from_str(raw, "%H:%M")
        .ok()
        .filter(|time| time.format("%H:%M").to_string() == raw)
        .ok_or("schedule time must be a valid HH:MM time")
}

fn schedule_weekday(weekday: Weekday) -> ScheduleWeekday {
    match weekday {
        Weekday::Mon => ScheduleWeekday::Monday,
        Weekday::Tue => ScheduleWeekday::Tuesday,
        Weekday::Wed => ScheduleWeekday::Wednesday,
        Weekday::Thu => ScheduleWeekday::Thursday,
        Weekday::Fri => ScheduleWeekday::Friday,
        Weekday::Sat => ScheduleWeekday::Saturday,
        Weekday::Sun => ScheduleWeekday::Sunday,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    #[test]
    fn next_times_follow_the_executing_devices_calendar() {
        let tz = FixedOffset::east_opt(8 * 3600).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 19, 23, 30, 0).unwrap(); // 周五 07:30
        let cases = [
            (
                ScheduleRule::Once {
                    date: "2026-11-20".into(),
                    time: "07:30".into(),
                },
                None,
            ),
            (
                ScheduleRule::Once {
                    date: "2026-11-21".into(),
                    time: "00:15".into(),
                },
                Some("2026-11-21T00:15:00+08:00"),
            ),
            (
                ScheduleRule::Daily {
                    time: "07:30".into(),
                },
                Some("2026-11-21T07:30:00+08:00"),
            ),
            (
                ScheduleRule::Weekly {
                    weekdays: vec![ScheduleWeekday::Friday, ScheduleWeekday::Monday],
                    time: "07:00".into(),
                },
                Some("2026-11-23T07:00:00+08:00"),
            ),
        ];
        for (rule, expected) in cases {
            let result = CalendarRule::parse(&rule)
                .unwrap()
                .next_after(&tz, now)
                .map(|time| time.to_rfc3339());
            assert_eq!(result.as_deref(), expected, "{rule:?}");
        }
    }

    #[test]
    fn validates_dates_times_and_weekday_sets() {
        let invalid = [
            ScheduleRule::Once {
                date: "2026-02-29".into(),
                time: "09:00".into(),
            },
            ScheduleRule::Daily {
                time: "9:00".into(),
            },
            ScheduleRule::Daily {
                time: "24:00".into(),
            },
            ScheduleRule::Weekly {
                weekdays: vec![],
                time: "08:00".into(),
            },
            ScheduleRule::Weekly {
                weekdays: vec![ScheduleWeekday::Monday, ScheduleWeekday::Monday],
                time: "08:00".into(),
            },
        ];
        for rule in invalid {
            assert!(CalendarRule::parse(&rule).is_err(), "{rule:?}");
        }
    }

    #[test]
    fn daylight_saving_skips_gaps_and_counts_folds_once() {
        let zone = SeasonalZone;
        let schedule = CalendarRule::parse(&ScheduleRule::Weekly {
            weekdays: vec![ScheduleWeekday::Sunday],
            time: "02:30".into(),
        })
        .unwrap();
        let before_gap = Utc.with_ymd_and_hms(2026, 3, 7, 12, 0, 0).unwrap();
        assert_eq!(
            schedule.next_after(&zone, before_gap).unwrap().to_rfc3339(),
            "2026-03-15T02:30:00-04:00"
        );

        let schedule = CalendarRule::parse(&ScheduleRule::Daily {
            time: "01:30".into(),
        })
        .unwrap();
        let before_fold = Utc.with_ymd_and_hms(2026, 11, 1, 5, 0, 0).unwrap();
        let between_occurrences = Utc.with_ymd_and_hms(2026, 11, 1, 6, 0, 0).unwrap();
        assert_eq!(
            schedule
                .next_after(&zone, before_fold)
                .unwrap()
                .to_rfc3339(),
            "2026-11-01T01:30:00-04:00"
        );
        assert_eq!(
            schedule
                .next_after(&zone, between_occurrences)
                .unwrap()
                .to_rfc3339(),
            "2026-11-02T01:30:00-05:00"
        );
    }

    /// 固定夹具的两次季节切换，避免测试使用机器自己的时区或外部时区数据库。
    #[derive(Clone)]
    struct SeasonalZone;

    impl TimeZone for SeasonalZone {
        type Offset = FixedOffset;

        fn from_offset(_: &Self::Offset) -> Self {
            Self
        }

        fn offset_from_local_date(
            &self,
            local: &NaiveDate,
        ) -> chrono::MappedLocalTime<Self::Offset> {
            self.offset_from_local_datetime(&local.and_hms_opt(12, 0, 0).unwrap())
        }

        fn offset_from_local_datetime(
            &self,
            local: &NaiveDateTime,
        ) -> chrono::MappedLocalTime<Self::Offset> {
            let summer = FixedOffset::west_opt(4 * 3600).unwrap();
            let winter = FixedOffset::west_opt(5 * 3600).unwrap();
            let matches = |offset: FixedOffset| {
                let utc = offset.from_local_datetime(local).unwrap().naive_utc();
                self.offset_from_utc_datetime(&utc) == offset
            };
            match (matches(summer), matches(winter)) {
                (true, true) => chrono::MappedLocalTime::Ambiguous(summer, winter),
                (true, false) => chrono::MappedLocalTime::Single(summer),
                (false, true) => chrono::MappedLocalTime::Single(winter),
                (false, false) => chrono::MappedLocalTime::None,
            }
        }

        fn offset_from_utc_date(&self, utc: &NaiveDate) -> Self::Offset {
            self.offset_from_utc_datetime(&utc.and_hms_opt(12, 0, 0).unwrap())
        }

        fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> Self::Offset {
            let spring = Utc
                .with_ymd_and_hms(2026, 3, 8, 7, 0, 0)
                .unwrap()
                .naive_utc();
            let autumn = Utc
                .with_ymd_and_hms(2026, 11, 1, 6, 0, 0)
                .unwrap()
                .naive_utc();
            let hours = if (spring..autumn).contains(utc) { 4 } else { 5 };
            FixedOffset::west_opt(hours * 3600).unwrap()
        }
    }
}
