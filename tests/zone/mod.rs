//! The zone the tests that stamp a piped time draw it in, so each says
//! the same zone and can say its stamp exactly.

/// A POSIX rule, which needs no zone database, as the build's sandbox has
/// none to look `UTC` up in. It is five and a half hours ahead of UTC,
/// off it by a part of an hour, so a stamp drawn in UTC, or with its
/// offset rounded to the hour, fails. Its stamp reads `+0530 (+05:30)`.
pub const ZONE: &str = "<+0530>-5:30";
