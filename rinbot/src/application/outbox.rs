#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    ConfirmedSuccess,
    ConfirmedFailure,
    Unknown,
}

pub fn may_retry_automatically(outcome: SendOutcome) -> bool {
    matches!(outcome, SendOutcome::ConfirmedFailure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_send_result_is_never_auto_retried() {
        assert!(!may_retry_automatically(SendOutcome::Unknown));
        assert!(may_retry_automatically(SendOutcome::ConfirmedFailure));
    }
}
