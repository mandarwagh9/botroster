//! Several sources of hub-served tools behind one [`InternalTools`].
//!
//! The hub holds a single internal-tools implementation. `bot.*`, connectors,
//! skills, and the credential broker are separate sources, so the composition
//! lives here rather than in the hub's routing.
//!
//! Two rules, both chosen because the alternative fails quietly:
//!
//! * Routing asks each source; it does not re-derive membership from the
//!   catalogue. A source may legitimately claim a name that is not in its
//!   catalogue right now: a connector that was unreachable at startup still
//!   owns its `<id>__*` namespace, so a call to it reports that the connector
//!   is down instead of falling through to the guest as an unknown tool.
//! * First registration wins, and a shadowed name is logged. Silently letting
//!   a later source shadow `bot.send` would allow a different implementation
//!   of an approved action to slip past review.

use std::collections::BTreeSet;
use std::sync::Arc;

use openbot_proto::frames::ToolDescription;
use serde_json::Value;

use crate::hub::InternalTools;

/// Fans one `InternalTools` interface out over several providers.
pub struct Composite {
    providers: Vec<Arc<dyn InternalTools>>,
}

impl Composite {
    pub fn new(providers: Vec<Arc<dyn InternalTools>>) -> Self {
        Self { providers }
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[async_trait::async_trait]
impl InternalTools for Composite {
    /// The union, in registration order, deduplicated first-wins.
    ///
    /// Duplicates are a configuration error rather than something to resolve
    /// silently: two providers offering `create_issue` means the model cannot
    /// tell which one it is calling.
    fn catalog(&self) -> Vec<ToolDescription> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for p in &self.providers {
            for t in p.catalog() {
                if seen.insert(t.tool_id.as_str().to_owned()) {
                    out.push(t);
                } else {
                    tracing::warn!(
                        tool = %t.tool_id.as_str(),
                        "two tool sources claim this name; keeping the first and ignoring the later one"
                    );
                }
            }
        }
        out
    }

    fn serves(&self, tool: &str) -> bool {
        self.providers.iter().any(|p| p.serves(tool))
    }

    async fn invoke(
        &self,
        as_bot: Option<&str>,
        tool: &str,
        args: &Value,
    ) -> Result<Value, String> {
        match self.providers.iter().find(|p| p.serves(tool)) {
            Some(p) => p.invoke(as_bot, tool, args).await,
            // Reached only if the hub's `serves` check and this one disagree,
            // which would be a bug here rather than a caller mistake.
            None => Err(format!("no tool source serves `{tool}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixed {
        name: &'static str,
        /// Names this source claims even though they are not in its catalogue,
        /// modelling a connector whose discovery failed.
        claims: Vec<&'static str>,
    }

    impl Fixed {
        fn new(name: &'static str) -> Self {
            Self {
                name,
                claims: vec![],
            }
        }
        fn claiming(name: &'static str, claims: Vec<&'static str>) -> Self {
            Self { name, claims }
        }
    }

    #[async_trait::async_trait]
    impl InternalTools for Fixed {
        fn catalog(&self) -> Vec<ToolDescription> {
            if self.claims.is_empty() {
                vec![ToolDescription::new(self.name, "", json!({}))]
            } else {
                vec![]
            }
        }
        fn serves(&self, tool: &str) -> bool {
            tool == self.name || self.claims.contains(&tool)
        }
        async fn invoke(&self, _: Option<&str>, tool: &str, _: &Value) -> Result<Value, String> {
            Ok(json!({ "served_by": self.name, "tool": tool }))
        }
    }

    fn composite(providers: Vec<Arc<dyn InternalTools>>) -> Composite {
        Composite::new(providers)
    }

    #[tokio::test]
    async fn the_catalogue_is_the_union_not_the_first_source() {
        let c = composite(vec![
            Arc::new(Fixed::new("bot.send")),
            Arc::new(Fixed::new("linear__create_issue")),
        ]);
        let ids: Vec<_> = c
            .catalog()
            .iter()
            .map(|t| t.tool_id.as_str().to_owned())
            .collect();
        assert_eq!(ids, vec!["bot.send", "linear__create_issue"]);
    }

    #[tokio::test]
    async fn each_source_gets_its_own_calls() {
        let c = composite(vec![
            Arc::new(Fixed::new("bot.send")),
            Arc::new(Fixed::new("linear__create_issue")),
        ]);
        for (tool, expected) in [
            ("bot.send", "bot.send"),
            ("linear__create_issue", "linear__create_issue"),
        ] {
            let v = c.invoke(None, tool, &json!({})).await.unwrap();
            assert_eq!(v["served_by"], expected);
        }
    }

    #[tokio::test]
    async fn a_later_source_cannot_shadow_an_earlier_one() {
        // The security-relevant case: a connector must not be able to take over
        // `bot.send`, which the operator has already approved by that name.
        let c = composite(vec![
            Arc::new(Fixed {
                name: "bot.send",
                claims: vec![],
            }),
            Arc::new(Fixed {
                name: "bot.send",
                claims: vec![],
            }),
        ]);
        assert_eq!(c.catalog().len(), 1, "a shadowed name was advertised twice");
        let v = c.invoke(None, "bot.send", &json!({})).await.unwrap();
        assert_eq!(v["served_by"], "bot.send");
    }

    #[tokio::test]
    async fn a_source_may_own_a_name_it_is_not_currently_advertising() {
        // A connector that failed discovery has an empty catalogue but still
        // owns its namespace, so the caller learns it is down rather than being
        // told the tool does not exist.
        let c = composite(vec![
            Arc::new(Fixed::new("bot.send")),
            Arc::new(Fixed::claiming("linear", vec!["linear__create_issue"])),
        ]);
        assert!(c.serves("linear__create_issue"));
        assert!(!c
            .catalog()
            .iter()
            .any(|t| t.tool_id.as_str() == "linear__create_issue"));
        let v = c.invoke(None, "linear__create_issue", &json!({})).await;
        assert_eq!(v.unwrap()["served_by"], "linear");
    }

    #[tokio::test]
    async fn an_unclaimed_tool_is_refused_rather_than_guessed() {
        let c = composite(vec![Arc::new(Fixed::new("bot.send"))]);
        assert!(!c.serves("fs.read"));
        let e = c.invoke(None, "fs.read", &json!({})).await.unwrap_err();
        assert!(e.contains("fs.read"), "{e}");
    }

    #[tokio::test]
    async fn an_empty_composite_serves_nothing_without_panicking() {
        let c = composite(vec![]);
        assert!(c.is_empty());
        assert!(c.catalog().is_empty());
        assert!(!c.serves("anything"));
        assert!(c.invoke(None, "anything", &json!({})).await.is_err());
    }
}
