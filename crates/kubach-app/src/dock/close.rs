//! Which tabs a command of the tab menu closes (#215).

/// A closing command of the right-click menu of a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CloseCommand {
    /// The tab under the pointer.
    This,
    /// Every tab but the one under the pointer.
    Others,
    /// The tabs after the one under the pointer.
    Right,
    /// The tabs before the one under the pointer.
    Left,
    /// Every tab.
    All,
}

impl CloseCommand {
    /// The commands in menu order.
    pub(super) const ALL: [Self; 5] =
        [Self::This, Self::Others, Self::Right, Self::Left, Self::All];
}

/// The tabs of `tabs` (in strip order) that `command` closes when the menu
/// was opened on the tab at `index`, in strip order. Empty when there is
/// nothing to close, and for an `index` outside the strip.
pub(super) fn tabs_to_close<T: Copy>(command: CloseCommand, tabs: &[T], index: usize) -> Vec<T> {
    if index >= tabs.len() {
        return Vec::new();
    }
    match command {
        CloseCommand::This => vec![tabs[index]],
        CloseCommand::Others => [&tabs[..index], &tabs[index + 1..]].concat(),
        CloseCommand::Right => tabs[index + 1..].to_vec(),
        CloseCommand::Left => tabs[..index].to_vec(),
        CloseCommand::All => tabs.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::{CloseCommand, tabs_to_close};

    const TABS: [u64; 4] = [10, 20, 30, 40];

    #[test]
    fn each_command_closes_its_side_of_the_clicked_tab() {
        assert_eq!(tabs_to_close(CloseCommand::This, &TABS, 1), [20]);
        assert_eq!(tabs_to_close(CloseCommand::Others, &TABS, 1), [10, 30, 40]);
        assert_eq!(tabs_to_close(CloseCommand::Right, &TABS, 1), [30, 40]);
        assert_eq!(tabs_to_close(CloseCommand::Left, &TABS, 1), [10]);
        assert_eq!(tabs_to_close(CloseCommand::All, &TABS, 1), TABS);
    }

    #[test]
    fn nothing_is_beyond_the_edges() {
        assert!(tabs_to_close(CloseCommand::Left, &TABS, 0).is_empty());
        assert_eq!(tabs_to_close(CloseCommand::Right, &TABS, 0), [20, 30, 40]);
        assert!(tabs_to_close(CloseCommand::Right, &TABS, 3).is_empty());
        assert_eq!(tabs_to_close(CloseCommand::Left, &TABS, 3), [10, 20, 30]);
    }

    /// With one tab only "Close" and "Close all" have something to close:
    /// the menu disables the rest.
    #[test]
    fn a_single_tab_has_no_others() {
        let tabs = [7_u64];
        for command in CloseCommand::ALL {
            let closes = !tabs_to_close(command, &tabs, 0).is_empty();
            let expected = matches!(command, CloseCommand::This | CloseCommand::All);
            assert_eq!(closes, expected, "{command:?}");
        }
    }

    /// The tab the menu was opened on has gone since: nothing closes.
    #[test]
    fn a_tab_outside_the_strip_closes_nothing() {
        for command in CloseCommand::ALL {
            assert!(tabs_to_close(command, &TABS, 4).is_empty(), "{command:?}");
            assert!(tabs_to_close::<u64>(command, &[], 0).is_empty());
        }
    }
}
