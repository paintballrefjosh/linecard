//! Linecard scripting.
//!
//! A script is a Python file that drives open sessions: send text, wait for
//! a prompt, read the result, open another connection. It runs in the user's
//! own Python (see [`python`]) as a separate process and talks to the app
//! through a private socket (see [`run`]). The app supplies the sessions
//! through the [`host::Host`] trait; [`tap::Tap`] is how it exposes a
//! session's output.

pub mod host;
pub mod python;
pub mod run;
pub mod tap;

pub use host::{ConnectionInfo, Host, SessionInfo};
pub use python::Python;
pub use run::{install_module, Event, Options, Outcome, Run};
pub use tap::Tap;
