//! The zone the tests that stamp a piped time draw it in, so each says
//! the same zone and can say its stamp exactly. These process tests prove
//! the zone and its offset; the unit case in src/swarm/tests.rs proves the
//! day, and keeps its own copy of the rule on purpose, since src/ can't
//! reach tests/.

/// A POSIX rule, which needs no zone database, as the build's sandbox has
/// none to look `UTC` up in. It is five and a half hours ahead of UTC,
/// off it by a part of an hour, so a stamp drawn in UTC, or with its
/// offset rounded to the hour, fails. Its stamp ends with [`STAMPED`].
pub const ZONE: &str = "<+0530>-5:30";

/// What a time drawn in [`ZONE`] ends with: its zone and its offset.
pub const STAMPED: &str = "+0530 (+05:30)";
