use crate::hap::pair_setup::PairSetupSession;
use crate::hap::pair_verify::PairVerifySession;
use crate::hap::runtime::Subscriptions;

pub(super) struct ConnectionState {
    pub pair_setup: PairSetupSession,
    pub pair_verify: PairVerifySession,
    pub subs: Subscriptions,
    /// The controller identifier learned from the most recent pair-verify on
    /// this connection. Only set after the channel becomes encrypted.
    pub controller_id: Option<String>,
}

impl ConnectionState {
    pub fn new() -> Self {
        Self {
            pair_setup: PairSetupSession::new(),
            pair_verify: PairVerifySession::new(),
            subs: Subscriptions::default(),
            controller_id: None,
        }
    }
}
