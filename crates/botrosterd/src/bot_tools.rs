//! `bot.*`, the hub's own tools.
//!
//! These are served by the control plane rather than the guest for two
//! reasons:
//!
//! 1. The guest does not own this state. A Bot's inbox lives with the
//!    control plane; the guest is a workspace with a filesystem and a browser.
//! 2. A handoff is an action. Putting work in another Bot's queue is
//!    consequential, and serving it here means it passes the same approval
//!    gate as a write or a shell command. A client-side implementation would
//!    be a check the caller could delete.

use std::sync::Arc;

use botroster_bots::{BotId, BotStore};
use botroster_proto::frames::ToolDescription;
use serde_json::{json, Value};

use crate::hub::InternalTools;

pub struct BotTools {
    bots: Arc<BotStore>,
}

impl BotTools {
    pub fn new(bots: Arc<BotStore>) -> Self {
        Self { bots }
    }
}

#[async_trait::async_trait]
impl InternalTools for BotTools {
    fn catalog(&self) -> Vec<ToolDescription> {
        vec![
            ToolDescription::new(
                "bot.list",
                "List the other Bots on this account, with what each one owns.",
                json!({ "type": "object", "properties": {}, "required": [] }),
            ),
            ToolDescription::new(
                "bot.send",
                "Hand work to another Bot. It picks the message up on its next run; \
                 you do not wait for a reply.",
                json!({
                    "type": "object",
                    "properties": {
                        "to": { "type": "string", "description": "The Bot's name or id." },
                        "message": {
                            "type": "string",
                            "description": "What you are handing over. Include everything \
                                            they need — they cannot see your conversation."
                        }
                    },
                    "required": ["to", "message"]
                }),
            ),
        ]
    }

    async fn invoke(
        &self,
        as_bot: Option<&str>,
        tool: &str,
        args: &Value,
    ) -> Result<Value, String> {
        match tool {
            "bot.list" => {
                let list = self
                    .bots
                    .list(false)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    // A Bot listing itself invites a self-handoff, which the
                    // store refuses anyway; do not suggest it.
                    .filter(|b| as_bot != Some(b.id.as_str()))
                    .map(|b| {
                        json!({
                            "id": b.id.as_str(),
                            "name": b.name,
                            "owns": b.title,
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(json!({ "bots": list }))
            }

            "bot.send" => {
                let to = args
                    .get("to")
                    .and_then(|v| v.as_str())
                    .ok_or("`to` must be a string")?;
                let message = args
                    .get("message")
                    .and_then(|v| v.as_str())
                    .ok_or("`message` must be a string")?;
                if message.trim().is_empty() {
                    return Err("`message` cannot be empty".into());
                }

                let dest = self.bots.resolve(to).map_err(|e| e.to_string())?;
                let from = as_bot.map(|b| BotId(b.to_owned()));
                self.bots
                    .send(from.as_ref(), &dest.id, message)
                    .map_err(|e| e.to_string())?;

                let waiting = self.bots.inbox(&dest.id).map_err(|e| e.to_string())?.len();
                Ok(json!({
                    "delivered_to": dest.id.as_str(),
                    "waiting": waiting,
                    "note": "they will pick this up on their next run; you are not waiting for a reply",
                }))
            }

            other => Err(format!("unknown internal tool `{other}`")),
        }
    }
}
