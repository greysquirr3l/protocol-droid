//! Driven adapters: the client transports and the injected side-effect ports.
//!
//! Populated by T13 (stdio, including child-process spawning), T14 and T15
//! (client-side HTTP transports) and T10 (the `Clock` and `IdSource` ports the
//! domain depends on but must never call globally).
