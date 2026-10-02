#[derive(Clone, Debug, Default)]
pub struct AccessPolicy {
    pub allow_groups: Vec<String>,
    pub allow_private: Vec<String>,
    pub deny_groups: Vec<String>,
    pub deny_private: Vec<String>,
    pub allow_all_when_empty: bool,
}

impl AccessPolicy {
    pub fn allows(&self, is_group: bool, peer_id: &str) -> bool {
        let peer_id = peer_id.trim();
        let (allow, deny) = if is_group {
            (&self.allow_groups, &self.deny_groups)
        } else {
            (&self.allow_private, &self.deny_private)
        };
        if deny.iter().any(|value| value == peer_id) {
            return false;
        }
        self.allow_all_when_empty && allow.is_empty() || allow.iter().any(|value| value == peer_id)
    }
}
