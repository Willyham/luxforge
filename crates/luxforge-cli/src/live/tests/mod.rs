//! The live client's own tests, against a [scripted session](scripted_session) and a real owner:
//! finding a session and speaking to it (`live_session_client_*`), and the `luxforge-ctl` command
//! line over it (`luxforge_ctl_commands_*`). The process tests, against the built binary, are
//! `tests/live_ctl`.
mod commands;
mod scripted_session;
mod session_and_client;
