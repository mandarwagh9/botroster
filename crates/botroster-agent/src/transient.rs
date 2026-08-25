//! Classification of failures into transient (retry shortly) and permanent
//! (needs a person).
//!
//! This is the same distinction the guest draws when a hub turns it away: an
//! outage ends on its own, a refusal does not. It matters most for a routine,
//! where nobody is watching and the next scheduled firing may be a day away;
//! without it, a nightly digest that hits a thirty-second hub restart waits
//! twenty-four hours.
//!
//! Unknown means permanent. Anything this module cannot positively identify
//! as an outage is left alone, so a wrong answer degrades to waiting for the
//! next scheduled run. Retrying something that will never succeed is worse:
//! it spends money on every attempt and buries the real problem under a log
//! of identical failures.

use botroster_proto::codes;

use crate::hub_client::HubError;
use crate::model::ModelError;

/// Whether a model failure is one that waiting can fix.
///
/// Decided on the typed error, because that is the only place the answer is
/// known. Once a failure has become a `FinishReason` it is a string, and
/// deciding from a string means searching it for "429", which is exactly what
/// [`crate::model::ModelError::Overloaded`] exists to avoid.
pub fn model_failure(e: &ModelError) -> bool {
    match e {
        ModelError::Transport(_) | ModelError::Overloaded(_) => true,
        // A bad key, an unknown model, a prompt too long for the context: all
        // still true tomorrow.
        _ => false,
    }
}

/// Whether this failure is one that waiting can fix.
pub fn is_transient(e: &anyhow::Error) -> bool {
    if let Some(h) = e.downcast_ref::<HubError>() {
        return match h {
            // The control plane is restarting, or was not up yet.
            HubError::Connect(_) | HubError::Closed => true,
            // No tool server bound: a guest reconnecting after its own
            // restart, which it does by itself within seconds.
            HubError::Rpc { code, .. } => *code == codes::NO_SERVER_BOUND,
            // Understood and refused. A protocol this hub does not speak will
            // not start speaking it in ten minutes, and a denied approval is a
            // decision somebody made.
            _ => false,
        };
    }
    if let Some(m) = e.downcast_ref::<ModelError>() {
        return model_failure(m);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transient(e: impl Into<anyhow::Error>) -> bool {
        is_transient(&e.into())
    }

    #[test]
    fn an_outage_is_worth_waiting_out() {
        assert!(transient(HubError::Connect("connection refused".into())));
        assert!(transient(HubError::Closed));
        assert!(transient(HubError::Rpc {
            code: codes::NO_SERVER_BOUND,
            message: "no tool server registered as `botroster-workspace`".into(),
        }));
        assert!(transient(ModelError::Transport("dns failure".into())));
        assert!(transient(ModelError::Overloaded(
            "HTTP 429: slow down".into()
        )));
    }

    #[test]
    fn a_decision_or_a_misconfiguration_is_not() {
        // Retrying these spends money to reach the same answer, and buries the
        // real problem under a log of identical failures.
        assert!(!transient(HubError::Refused(
            "unsupported protocol_version".into()
        )));
        assert!(!transient(HubError::Rpc {
            code: codes::APPROVAL_DENIED,
            message: "denied by the approver".into(),
        }));
        assert!(!transient(ModelError::Rejected(
            "HTTP 401: incorrect api key".into()
        )));
    }

    #[test]
    fn something_this_does_not_recognise_is_left_alone() {
        // The safe default is to wait for the next scheduled run rather than
        // retry something unrecognised.
        assert!(!transient(anyhow::anyhow!("a disk somewhere is full")));
    }
}
