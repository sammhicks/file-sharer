pub struct FilenameTimestamp(time::OffsetDateTime);

impl std::fmt::Display for FilenameTimestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let year = self.0.year();
        let month = self.0.month() as u8;
        let day = self.0.day();
        let hour = self.0.hour();
        let minute = self.0.minute();

        write!(f, "{year:04}{month:02}{day:02}{hour:02}{minute:02}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(time::OffsetDateTime);

impl Timestamp {
    pub fn now() -> Result<Self, time::error::IndeterminateOffset> {
        time::OffsetDateTime::now_local().map(Self)
    }

    pub fn into_filename(self) -> FilenameTimestamp {
        FilenameTimestamp(self.0)
    }
}

impl std::ops::Add<time::Duration> for Timestamp {
    type Output = Self;

    fn add(mut self, rhs: time::Duration) -> Self::Output {
        self.0 += rhs;

        self
    }
}

impl<'de> serde::Deserialize<'de> for Timestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use toml::value::Datetime;

        fn toml_date<E: serde::de::Error>(date: toml::value::Date) -> Result<time::Date, E> {
            time::Date::from_calendar_date(
                date.year.into(),
                date.month
                    .try_into()
                    .map_err(|err| E::custom(format!("Bad Month: {}", err)))?,
                date.day,
            )
            .map_err(|err| E::custom(format!("Bad Date: {}", err)))
        }

        fn toml_time<E: serde::de::Error>(time: toml::value::Time) -> Result<time::Time, E> {
            time::Time::from_hms(time.hour, time.minute, time.second)
                .map_err(|err| E::custom(format!("Bad Time: {}", err)))
        }

        fn toml_offset<E: serde::de::Error>(
            offset: toml::value::Offset,
        ) -> Result<time::UtcOffset, E> {
            Ok(match offset {
                toml::value::Offset::Z => time::UtcOffset::UTC,
                toml::value::Offset::Custom { hours, minutes } => time::UtcOffset::from_hms(
                    hours,
                    minutes
                        .try_into()
                        .map_err(|err| E::custom(format!("Bad Minutes: {}", err)))?,
                    0,
                )
                .map_err(|err| E::custom(format!("Bad Time: {}", err)))?,
            })
        }

        Ok(Self(match Datetime::deserialize(deserializer)? {
            Datetime {
                date: Some(date),
                time: Some(time),
                offset: Some(offset),
            } => time::PrimitiveDateTime::new(toml_date(date)?, toml_time(time)?)
                .assume_offset(toml_offset(offset)?),

            datetime => {
                return Err(<D::Error as serde::de::Error>::custom(format!(
                    "Invalid datetime: {}. You must specify date, time, and offset from UTC",
                    datetime
                )))
            }
        }))
    }
}

impl serde::Serialize for Timestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        fn toml_date<E: serde::ser::Error>(date: time::Date) -> Result<toml::value::Date, E> {
            Ok(toml::value::Date {
                year: date
                    .year()
                    .try_into()
                    .map_err(|err| E::custom(format!("Bad year: {}", err)))?,
                month: date.month().into(),
                day: date.day(),
            })
        }

        fn toml_time(date: time::Time) -> toml::value::Time {
            toml::value::Time {
                hour: date.hour(),
                minute: date.minute(),
                second: date.second(),
                nanosecond: 0,
            }
        }

        fn toml_offset<E: serde::ser::Error>(
            date: time::UtcOffset,
        ) -> Result<toml::value::Offset, E> {
            let (hours, minutes, _seconds) = date.as_hms();
            Ok(toml::value::Offset::Custom {
                hours,
                minutes: minutes
                    .try_into()
                    .map_err(|err| E::custom(format!("Bad minutes: {}", err)))?,
            })
        }

        toml::value::Datetime {
            date: Some(toml_date(self.0.date())?),
            time: Some(toml_time(self.0.time())),
            offset: Some(toml_offset(self.0.offset())?),
        }
        .serialize(serializer)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WebTimestamp(time::OffsetDateTime);

impl WebTimestamp {
    pub fn now() -> Result<Self, time::error::IndeterminateOffset> {
        time::OffsetDateTime::now_local().map(Self)
    }
}

impl From<Timestamp> for WebTimestamp {
    fn from(Timestamp(timestamp): Timestamp) -> Self {
        Self(timestamp)
    }
}

impl From<WebTimestamp> for Timestamp {
    fn from(WebTimestamp(timestamp): WebTimestamp) -> Self {
        Self(timestamp)
    }
}

impl std::fmt::Display for WebTimestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use time::format_description::well_known::{iso8601, Iso8601};

        const FORMAT: iso8601::EncodedConfig = iso8601::Config::DEFAULT
            .set_time_precision(iso8601::TimePrecision::Minute {
                decimal_digits: None,
            })
            .set_formatted_components(iso8601::FormattedComponents::DateTime)
            .encode();

        self.0
            .format(&Iso8601::<FORMAT>)
            .map_err(|_| std::fmt::Error)?
            .fmt(f)
    }
}

impl<'de> serde::Deserialize<'de> for WebTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let datetime = time::PrimitiveDateTime::parse(
            &String::deserialize(deserializer)?,
            &time::format_description::well_known::Iso8601::PARSING,
        )
        .map_err(|err| <D::Error as serde::de::Error>::custom(format!("Bad timestamp: {err}")))?;

        let offset = time::UtcOffset::current_local_offset()
            .map_err(|err| <D::Error as serde::de::Error>::custom(format!("Bad offset: {err}")))?;

        Ok(Self(datetime.assume_offset(offset)))
    }
}

impl std::ops::Add<time::Duration> for WebTimestamp {
    type Output = Self;

    fn add(mut self, rhs: time::Duration) -> Self::Output {
        self.0 += rhs;

        self
    }
}
