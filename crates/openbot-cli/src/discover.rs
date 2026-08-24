//! Finding a model that is already on this machine, without being told.
//!
//! The first thing a new person met used to be a refusal:
//!
//! ```text
//! Error: no model configured.
//! Set one once:  openbot config set --model grok-4-5
//! ```
//!
//! which is an instruction to go and get an account somewhere, come back, and
//! type a command with four flags. Meanwhile a great many of the people who
//! would try this already have Ollama or LM Studio running, with a model
//! downloaded, on a port this process can reach in under a second. Asking them
//! to configure what is already there is asking them to do the computer's job.
//!
//! Nothing here is a fallback in the sense of "second best". A local model on
//! localhost needs no account, no key, and sends nothing off the machine, which
//! is the arrangement this project's whole position argues for.
//!
//! # What this deliberately does not do
//!
//! It does not go looking on the network. Every address probed is loopback, and
//! that is not a performance decision: a tool that scans for open ports on the
//! machines around it is doing something its user did not ask for and would not
//! like, whatever it finds.

use std::time::Duration;

/// How long to wait for a local server to answer.
///
/// Loopback, so an answer is immediate or there is nothing there. Long enough
/// for a busy machine, short enough that four of these in parallel do not
/// register as a pause before a command someone typed.
const PROBE: Duration = Duration::from_millis(700);

/// A model found running on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// What to pass as `--model`.
    pub model: String,
    /// The OpenAI-compatible base URL, ready for `--base-url`.
    pub base_url: String,
    /// What is serving it, for the sentence a person reads.
    pub served_by: &'static str,
    /// How many models that server offers, so the message can say there are
    /// others without listing all of them.
    pub also: usize,
}

impl Discovered {
    /// The command that would make this permanent.
    ///
    /// Printed rather than run: adopting a model for one command is a
    /// convenience, and writing to someone's config is a decision.
    pub fn to_config_command(&self) -> String {
        format!(
            "openbot config set --model {} --dialect openai --base-url {} --api-key-env ''",
            self.model, self.base_url
        )
    }
}

/// A place a local model server might be listening, and how to ask it.
struct Probe {
    /// `served_by` for the message.
    name: &'static str,
    /// Where to ask what it has.
    list: &'static str,
    /// What to hand the agent afterwards.
    base_url: &'static str,
    /// Ollama's native listing is not the OpenAI one, and it is the server most
    /// people have; the rest all answer `/v1/models`.
    ollama_shape: bool,
}

const PROBES: &[Probe] = &[
    Probe {
        name: "Ollama",
        list: "http://localhost:11434/api/tags",
        base_url: "http://localhost:11434/v1",
        ollama_shape: true,
    },
    Probe {
        name: "LM Studio",
        list: "http://localhost:1234/v1/models",
        base_url: "http://localhost:1234/v1",
        ollama_shape: false,
    },
    Probe {
        name: "a local server",
        list: "http://localhost:8080/v1/models",
        base_url: "http://localhost:8080/v1",
        ollama_shape: false,
    },
    Probe {
        name: "a local server",
        list: "http://localhost:8000/v1/models",
        base_url: "http://localhost:8000/v1",
        ollama_shape: false,
    },
];

/// Pull the model names out of whichever listing shape this server speaks.
///
/// Split from the request so the parsing can be tested without a server. Both
/// shapes are a list of objects with a name field; they disagree about what the
/// list and the field are called.
fn models_in(body: &serde_json::Value, ollama_shape: bool) -> Vec<String> {
    let (list, field) = if ollama_shape {
        ("models", "name")
    } else {
        ("data", "id")
    };
    body.get(list)
        .and_then(|m| m.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|m| m.get(field).and_then(|n| n.as_str()))
                .filter(|n| !n.trim().is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Ask one server what it has.
async fn ask(client: &reqwest::Client, p: &Probe) -> Option<Discovered> {
    let body: serde_json::Value = client
        .get(p.list)
        .timeout(PROBE)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let names = models_in(&body, p.ollama_shape);
    let first = names.first()?;
    Some(Discovered {
        model: first.clone(),
        base_url: p.base_url.to_owned(),
        served_by: p.name,
        also: names.len().saturating_sub(1),
    })
}

/// A model already running on this machine, if there is one.
///
/// Probed in parallel and in order of how likely each is, so the answer arrives
/// in about one round trip rather than four. The order matters when more than
/// one is running: it is a preference, not a race, and a race would pick a
/// different model on different days for no reason a person could see.
pub async fn local_model() -> Option<Discovered> {
    let Ok(client) = reqwest::Client::builder().timeout(PROBE).build() else {
        return None;
    };
    let mut asking: Vec<_> = PROBES.iter().map(|p| ask(&client, p)).collect();
    let answers = futures_util::future::join_all(asking.drain(..)).await;
    answers.into_iter().flatten().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ollama_and_openai_listings_are_both_understood() {
        // The two shapes, as the two servers actually return them. Ollama is
        // the one most people have and is the one that is not OpenAI-shaped.
        let ollama = json!({ "models": [
            { "name": "qwen3:1.7b", "size": 1 },
            { "name": "llama3.2:3b", "size": 2 }
        ]});
        assert_eq!(
            models_in(&ollama, true),
            vec!["qwen3:1.7b".to_owned(), "llama3.2:3b".to_owned()]
        );

        let openai = json!({ "data": [
            { "id": "local-model", "object": "model" }
        ]});
        assert_eq!(models_in(&openai, false), vec!["local-model".to_owned()]);
    }

    #[test]
    fn a_listing_read_with_the_wrong_shape_finds_nothing() {
        // Rather than finding something wrong. Reading Ollama's reply as an
        // OpenAI one must not produce a model id from some other field, because
        // a made-up id fails later, further away, with a vendor's error.
        let ollama = json!({ "models": [{ "name": "qwen3:1.7b" }] });
        assert!(models_in(&ollama, false).is_empty());
        let openai = json!({ "data": [{ "id": "local-model" }] });
        assert!(models_in(&openai, true).is_empty());
    }

    #[test]
    fn nothing_useful_is_taken_from_an_empty_or_unexpected_reply() {
        for body in [
            json!({}),
            json!({ "models": [] }),
            json!({ "data": [] }),
            json!({ "models": [{ "size": 1 }] }),
            json!({ "models": [{ "name": "" }] }),
            json!({ "models": "not a list" }),
            json!([1, 2, 3]),
        ] {
            assert!(
                models_in(&body, true).is_empty() && models_in(&body, false).is_empty(),
                "took a model name out of {body}"
            );
        }
    }

    #[test]
    fn the_suggested_command_is_one_that_would_work() {
        // An empty `--api-key-env` is what says "this endpoint wants no
        // credential", and without it the command it prints would fail on a
        // missing key for a model on localhost that never needed one.
        let d = Discovered {
            model: "qwen3:1.7b".into(),
            base_url: "http://localhost:11434/v1".into(),
            served_by: "Ollama",
            also: 1,
        };
        let cmd = d.to_config_command();
        assert!(cmd.contains("--model qwen3:1.7b"), "{cmd}");
        assert!(
            cmd.contains("--base-url http://localhost:11434/v1"),
            "{cmd}"
        );
        assert!(cmd.contains("--api-key-env ''"), "{cmd}");
    }
}
