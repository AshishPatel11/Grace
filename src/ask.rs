//! "Ask Claude about this": send selected text from any tab to a Claude tab.
//!
//! Select an error in a PowerShell tab, right-click, "Ask Claude about this" > pick the Claude
//! tab. The text is pasted into that Claude's input (in a code block, with where it came
//! from), and that tab comes to the front. Nothing is sent yet: you type your question and
//! press Enter yourself.

use crate::app::{App, SUB_END, SUB_START};

/// Menu ids from here up pick the Claude tab (base + index in `Ui::ask_targets`).
pub const ASK_BASE: u32 = 500;

/// The text as it is pasted: where it came from, then the text in a code block.
pub fn quote(from: &str, text: &str) -> String {
    let text = text.trim_end();
    // A ``` inside the text would end the block early: use ~~~ then.
    let fence = if text.contains("```") { "~~~" } else { "```" };
    format!("From the \"{from}\" tab:\n{fence}\n{text}\n{fence}\n")
}

impl App {
    /// The "Ask Claude about this" submenu of the right-click menu, when text is selected
    /// in pane `id`. Lists the running Claude tabs.
    pub fn ask_menu_items(&mut self, id: u64) -> Vec<(u32, String)> {
        if self.selection_text(id).is_none() {
            return Vec::new();
        }
        let mut targets = Vec::new();
        let mut items = vec![(SUB_START, "Ask Claude about this".to_string())];
        for (ti, t) in self.tabs.iter().enumerate() {
            for p in t.root.leaves() {
                if !self.panes.get(&p).is_some_and(|x| x.live.is_some() && x.is_claude()) {
                    continue;
                }
                let here = if p == id { "   (this tab)" } else { "" };
                // "&" would underline the next letter in a Windows menu.
                let label = format!("{}  {}{here}", ti + 1, self.tab_title(ti).replace('&', "&&"));
                items.push((ASK_BASE + targets.len() as u32, label));
                targets.push(p);
            }
        }
        if targets.is_empty() {
            return Vec::new();
        }
        items.push((SUB_END, String::new()));
        self.ui.ask_targets = targets;
        items
    }

    /// A pick from the submenu: paste pane `id`'s selection into that Claude tab.
    pub fn ask_menu_pick(&mut self, id: u64, cmd: u32) {
        let Some(target) = cmd.checked_sub(ASK_BASE).and_then(|i| self.ui.ask_targets.get(i as usize).copied()) else { return };
        let Some(text) = self.selection_text(id) else {
            self.show_toast("Select some text first.".into());
            return;
        };
        let from = self.tab_of(id).map(|ti| self.tab_title(ti)).unwrap_or_else(|| "other".into());
        let body = quote(&from, &text);
        // Bring that Claude to the front, then paste (into its Rich input box when open).
        if let Some(ti) = self.tab_of(target) {
            self.activate(ti);
        }
        self.focus_pane(target);
        if self.box_takes_keys() == Some(target) {
            self.box_insert(target, &body);
        } else {
            self.paste_text(target, body, false);
        }
        let to = self.tab_of(target).map(|ti| self.tab_title(ti)).unwrap_or_default();
        self.show_toast(format!("Pasted into \"{to}\". Type your question, then press Enter."));
        self.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_the_text_in_a_code_block() {
        assert_eq!(quote("PowerShell", "error: x\n\n"), "From the \"PowerShell\" tab:\n```\nerror: x\n```\n");
        assert!(quote("a", "use ```this```").contains("~~~\nuse ```this```\n~~~"));
    }
}
