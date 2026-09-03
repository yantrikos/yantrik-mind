#[cfg(test)]
mod page1_tests {
    use crate::required_filename;

    // ── a requirement is honoured ────────────────────────────────────────────────────────────
    #[test]
    fn an_explicitly_required_filename_is_found() {
        // General phrasings first. The last one is the frozen T2 brief's, included openly: the
        // defect it exposed is one any user hits, and the baseline it will be measured against is
        // already recorded, so the test is written where a reader can see both.
        for (task, want) in [
            ("Build me a landing page. Name it launch.html.", "launch.html"),
            ("Make a site; the entry point is index.html", "index.html"),
            ("Write the page and save as portfolio.html please", "portfolio.html"),
            ("the filename must be about-us.html", "about-us.html"),
            ("Put resume.html in the project root", "resume.html"),
            ("call it notes_2026.html", "notes_2026.html"),
            (
                "Build a personal website for a software engineer. Static files only, entry \
                 `index.html` in the project root; no build step.",
                "index.html",
            ),
        ] {
            assert_eq!(
                required_filename(task).as_deref(),
                Some(want),
                "task: {task}"
            );
        }
    }

    #[test]
    fn the_requirement_may_be_stated_before_or_after_the_name() {
        assert_eq!(
            required_filename("index.html is the entry point").as_deref(),
            Some("index.html")
        );
        assert_eq!(
            required_filename("the entry point is index.html").as_deref(),
            Some("index.html")
        );
    }

    // ── kill criterion 1: nothing changes without a requirement ──────────────────────────────
    #[test]
    fn a_task_with_no_filename_requirement_finds_nothing() {
        for task in [
            "Build me a portfolio site for a photographer",
            "make a page about the history of the bicycle",
            "a one-pager for a bakery, warm and simple",
            "",
        ] {
            assert_eq!(required_filename(task), None, "task: {task}");
        }
    }

    // ── kill criterion 2: an incidental mention is not a requirement ─────────────────────────
    #[test]
    fn an_incidental_mention_does_not_capture_the_name() {
        for task in [
            "See index.html for an example of the style I like",
            "unlike about.html, this one should be playful",
            "the old site had a broken contact.html and I never fixed it",
        ] {
            assert_eq!(required_filename(task), None, "task: {task}");
        }
    }

    // ── kill criterion 3: ambiguity falls back rather than guessing ──────────────────────────
    #[test]
    fn two_different_required_names_fall_back_instead_of_picking_one() {
        let task = "The entry must be index.html and the filename for the second page must be \
                    about.html";
        assert_eq!(
            required_filename(task),
            None,
            "with two requirements the honest answer is none: a confident wrong name is worse \
             than the predictable title slug"
        );
        // The SAME name twice is not ambiguity — it is one requirement stated twice.
        assert_eq!(
            required_filename("entry index.html; index.html must be at the root").as_deref(),
            Some("index.html")
        );
    }

    // ── kill criterion 4: a name may never be a path ─────────────────────────────────────────
    #[test]
    fn a_required_name_can_never_escape_the_web_directory() {
        // Each of these names a file with a cue right beside it. The stem walk stops at a slash,
        // so what comes back is the bare tail or nothing — never a traversal. This is a security
        // property, not a naming one: the name reaches the filesystem.
        for task in [
            "save as ../../etc/passwd.html",
            "the entry must be /etc/cron.d/evil.html",
            "name it ..\\..\\windows\\system32\\x.html",
            "call it subdir/index.html",
        ] {
            let got = required_filename(task);
            if let Some(name) = got {
                assert!(
                    !name.contains('/')
                        && !name.contains(char::from(92))
                        && !name.contains("..")
                        && !name.starts_with('.'),
                    "a path escaped into the filename: {name:?} from {task:?}"
                );
            }
        }
    }

    #[test]
    fn a_dot_run_inside_a_name_is_refused_and_that_guard_can_fire() {
        // The stem walk stops at a slash, so a traversal never reaches the `..` check. A dot does
        // NOT stop the walk, so this is the input that makes that guard observable — without it
        // the guard would be a third piece of unreachable defence.
        assert_eq!(required_filename("name it my..page.html"), None);
        assert_eq!(required_filename("save as ..hidden.html"), None);
        // ...while an ordinary dot in a name is fine.
        assert_eq!(
            required_filename("name it v1.2.report.html").as_deref(),
            Some("v1.2.report.html")
        );
    }

    #[test]
    fn a_name_too_short_to_be_a_name_is_refused() {
        // ".html" alone, and a bare dotfile, are not filenames a task can require.
        assert_eq!(required_filename("name it .html"), None);
        assert_eq!(required_filename("save as .html"), None);
    }
}

#[cfg(test)]
mod page1_recipe_tests {
    use crate::delegate::page_recipe;

    /// The unit rule being right is not the same as the recipe using it. This walks the recipe the
    /// delegation actually runs and reads the publish step's arguments.
    fn publish_args(task: &str) -> serde_json::Value {
        let r = page_recipe("t", task, None);
        for step in &r.steps {
            if let mind_recipes::RecipeStep::Tool {
                tool_name, args, ..
            } = step
            {
                if tool_name == "publish_page" {
                    return args.clone();
                }
            }
        }
        panic!("the page recipe has no publish_page step");
    }

    #[test]
    fn a_required_filename_reaches_the_publish_call() {
        let a = publish_args(
            "Build a personal website for a software engineer. Static files only, entry              `index.html` in the project root.",
        );
        assert_eq!(
            a.get("required_filename").and_then(|v| v.as_str()),
            Some("index.html"),
            "the brief's requirement must travel with the publish call: {a}"
        );
    }

    #[test]
    fn a_brief_with_no_requirement_carries_no_filename_and_is_unchanged() {
        let a = publish_args("Build me a portfolio site for a photographer");
        assert!(
            a.get("required_filename").is_none(),
            "nothing may be invented when the brief asks for nothing: {a}"
        );
        assert!(a.get("name").is_some() && a.get("html").is_some());
    }
}

