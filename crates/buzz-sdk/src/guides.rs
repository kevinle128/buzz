//! On-demand agent guidance shared by `buzz guide` and the `buzz-acp` base prompt.
//!
//! The base prompt keeps the rules an agent needs on every turn. Guidance that
//! is only needed for one kind of work lives here as one markdown file per
//! topic, so the base prompt can point to it with a one-line trigger and the
//! CLI can print the same text without a relay connection.

/// One guide topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guide {
    /// Topic name used as `buzz guide <name>`.
    pub name: &'static str,
    /// When to open the topic. The base prompt index line is
    /// ``- <trigger>: run `buzz guide <name>`.``
    pub trigger: &'static str,
    /// Full markdown text of the topic, including its heading.
    pub text: &'static str,
}

/// Every guide topic, in the order the index lists them.
pub const GUIDES: &[Guide] = &[
    Guide {
        name: "projects",
        trigger: "Before creating or changing a project, repository, issue or pull request, announcing one with its link, or assigning an issue",
        text: include_str!("guides/projects.md"),
    },
    Guide {
        name: "agent-creation",
        trigger: "When someone asks you to create an agent or change an existing personal agent",
        text: include_str!("guides/agent-creation.md"),
    },
    Guide {
        name: "memory",
        trigger: "Before you write, trim, link or remove memory (`core` or `buzz mem`)",
        text: include_str!("guides/memory.md"),
    },
    Guide {
        name: "workspace",
        trigger: "When you need to know where your files, plans, logs or repository checkouts belong",
        text: include_str!("guides/workspace.md"),
    },
    Guide {
        name: "repo-work",
        trigger: "Before you plan or edit in a repository or worktree, or commit",
        text: include_str!("guides/repo-work.md"),
    },
];

/// Look up a topic by its exact name.
pub fn find(name: &str) -> Option<&'static Guide> {
    GUIDES.iter().find(|guide| guide.name == name)
}

/// The topic list: one `name — trigger` line per topic.
pub fn list() -> String {
    GUIDES
        .iter()
        .map(|guide| format!("{} — {}\n", guide.name, guide.trigger))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_names_are_unique_and_texts_start_with_a_heading() {
        for (i, guide) in GUIDES.iter().enumerate() {
            assert!(
                GUIDES[i + 1..].iter().all(|other| other.name != guide.name),
                "duplicate topic {}",
                guide.name
            );
            assert!(
                guide.text.starts_with("## "),
                "topic {} must start with its heading",
                guide.name
            );
        }
    }

    #[test]
    fn find_returns_the_exact_topic_only() {
        assert_eq!(find("memory").map(|g| g.name), Some("memory"));
        assert!(find("Memory").is_none());
        assert!(find("").is_none());
    }

    #[test]
    fn list_names_every_topic_with_its_trigger() {
        let list = list();
        for guide in GUIDES {
            assert!(list.contains(&format!("{} — {}", guide.name, guide.trigger)));
        }
    }
}
