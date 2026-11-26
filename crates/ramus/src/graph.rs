//! Lays out the commit graph: which lane each commit's dot sits in, and which lines run
//! through each row. Pure, so the layout is testable without drawing anything.

/// A line through one half of a row, from one lane to another (the same lane for a
/// straight line).
pub type Segment = (usize, usize);

/// What one commit's row of the graph shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphRow {
    /// The lane the commit's dot sits in.
    pub lane: usize,
    /// Lines in the top half: from a lane at the row's top edge to a lane at the dot's
    /// height.
    pub top: Vec<Segment>,
    /// Lines in the bottom half: from a lane at the dot's height to a lane at the row's
    /// bottom edge.
    pub bottom: Vec<Segment>,
    /// The commit has more than one parent.
    pub merge: bool,
}

impl GraphRow {
    /// How many lanes the row uses: one past the rightmost lane anything touches.
    pub fn width(&self) -> usize {
        self.top
            .iter()
            .chain(&self.bottom)
            .flat_map(|(from, to)| [*from, *to])
            .chain([self.lane])
            .max()
            .map_or(0, |lane| lane + 1)
    }
}

/// The lanes open after the rows laid out so far, each waiting for the commit it leads
/// to. Kept between pages, so the next page carries on where the last one ended.
#[derive(Clone, Debug, Default)]
pub struct Graph {
    lanes: Vec<Option<String>>,
}

impl Graph {
    /// Lays out the next commit in History's order (children before parents).
    pub fn push(&mut self, id: &str, parents: &[String]) -> GraphRow {
        let incoming: Vec<usize> = self.lanes_heading_to(id).collect();
        // The dot goes in the leftmost lane that leads to it; a branch tip nothing leads
        // to starts in the first free lane.
        let lane = match incoming.first() {
            Some(&lane) => lane,
            None => self.free_lane(),
        };
        let top = self
            .lanes
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.is_some())
            .map(|(i, slot)| (i, if slot.as_deref() == Some(id) { lane } else { i }))
            .collect();
        for &i in &incoming {
            self.lanes[i] = None;
        }

        // The first parent carries on in the dot's lane. Another parent joins a lane
        // already heading to it, or opens a new one.
        let mut started = Vec::new();
        let mut joined = Vec::new();
        if let Some(first) = parents.first() {
            self.lanes[lane] = Some(first.clone());
            started.push(lane);
        }
        for parent in parents.iter().skip(1) {
            let existing = self.lanes_heading_to(parent).next();
            match existing {
                Some(existing) => joined.push(existing),
                None => {
                    let new = self.free_lane();
                    self.lanes[new] = Some(parent.clone());
                    started.push(new);
                }
            }
        }

        let mut bottom: Vec<Segment> = self
            .lanes
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.is_some())
            .map(|(i, _)| {
                if started.contains(&i) {
                    (lane, i)
                } else {
                    (i, i)
                }
            })
            .collect();
        bottom.extend(joined.into_iter().map(|existing| (lane, existing)));
        while self.lanes.last().is_some_and(Option::is_none) {
            self.lanes.pop();
        }

        GraphRow {
            lane,
            top,
            bottom,
            merge: parents.len() > 1,
        }
    }

    fn lanes_heading_to<'a>(&'a self, id: &'a str) -> impl Iterator<Item = usize> + 'a {
        self.lanes
            .iter()
            .enumerate()
            .filter(move |(_, slot)| slot.as_deref() == Some(id))
            .map(|(i, _)| i)
    }

    /// The first empty lane, adding one at the right if every lane is taken.
    fn free_lane(&mut self) -> usize {
        match self.lanes.iter().position(Option::is_none) {
            Some(free) => free,
            None => {
                self.lanes.push(None);
                self.lanes.len() - 1
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|id| (*id).to_owned()).collect()
    }

    /// Lays out `commits` (id, parents), newest first, as History lists them.
    fn layout(commits: &[(&str, &[&str])]) -> Vec<GraphRow> {
        let mut graph = Graph::default();
        commits
            .iter()
            .map(|(id, parents)| graph.push(id, &ids(parents)))
            .collect()
    }

    #[test]
    fn a_straight_history_stays_in_one_lane() {
        let rows = layout(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]);

        assert!(rows.iter().all(|row| row.lane == 0 && row.width() == 1));
        assert_eq!(
            (rows[0].top.clone(), rows[0].bottom.clone()),
            (vec![], vec![(0, 0)])
        );
        assert_eq!(
            (rows[1].top.clone(), rows[1].bottom.clone()),
            (vec![(0, 0)], vec![(0, 0)])
        );
        assert_eq!(
            (rows[2].top.clone(), rows[2].bottom.clone()),
            (vec![(0, 0)], vec![])
        );
    }

    #[test]
    fn a_merged_branch_splits_off_and_rejoins() {
        // m merges s2 into b; s2 and b both come from a.
        let rows = layout(&[
            ("m", &["b", "s2"]),
            ("s2", &["s1"]),
            ("s1", &["a"]),
            ("b", &["a"]),
            ("a", &[]),
        ]);

        assert!(rows[0].merge);
        assert_eq!(
            rows[0].bottom,
            [(0, 0), (0, 1)],
            "the merge opens a second lane"
        );
        assert_eq!(rows[1].lane, 1, "the branch runs in its own lane");
        assert_eq!(rows[2].lane, 1);
        assert_eq!(rows[3].lane, 0, "main carries on in the first lane");
        assert_eq!(rows[3].top, [(0, 0), (1, 1)]);
        assert_eq!(rows[4].lane, 0);
        assert_eq!(rows[4].top, [(0, 0), (1, 0)], "both lanes meet at the fork");
        assert!(rows[4].bottom.is_empty());
    }

    #[test]
    fn unrelated_histories_each_start_in_the_first_free_lane() {
        let rows = layout(&[("b", &[]), ("a", &[])]);

        assert_eq!((rows[0].lane, rows[1].lane), (0, 0));
        assert!(rows[1].top.is_empty(), "nothing above leads to a");
    }

    #[test]
    fn a_branch_tip_with_no_child_above_starts_a_new_lane() {
        // All branches: x is another branch's tip, listed between m and its parent.
        let rows = layout(&[("m", &["a"]), ("x", &["a"]), ("a", &[])]);

        assert_eq!(rows[1].lane, 1);
        assert_eq!(
            rows[1].top,
            [(0, 0)],
            "only main's line passes; nothing enters x"
        );
        assert_eq!(rows[2].top, [(0, 0), (1, 0)]);
    }

    #[test]
    fn an_octopus_merge_opens_a_lane_per_parent() {
        let rows = layout(&[
            ("m", &["p1", "p2", "p3"]),
            ("p3", &[]),
            ("p2", &[]),
            ("p1", &[]),
        ]);

        assert_eq!(rows[0].bottom, [(0, 0), (0, 1), (0, 2)]);
        assert_eq!(rows[0].width(), 3);
    }

    #[test]
    fn a_merge_whose_parent_is_already_expected_joins_that_lane() {
        // m2 merges b, which another lane is already heading to.
        let rows = layout(&[
            ("m1", &["a", "b"]),
            ("m2", &["a", "b"]),
            ("b", &["a"]),
            ("a", &[]),
        ]);

        assert_eq!(rows[1].lane, 2, "m2 is a separate tip");
        assert!(
            rows[1].bottom.contains(&(2, 1)),
            "it joins the lane already going to b"
        );
        assert!(rows[1].bottom.contains(&(1, 1)), "and that lane carries on");
    }

    #[test]
    fn laying_out_in_pages_matches_laying_out_at_once() {
        let commits: [(&str, &[&str]); 5] = [
            ("m", &["b", "s"]),
            ("s", &["a"]),
            ("b", &["a"]),
            ("a", &["root"]),
            ("root", &[]),
        ];
        let whole = layout(&commits);

        let mut graph = Graph::default();
        let mut paged = Vec::new();
        for page in commits.chunks(2) {
            for (id, parents) in page {
                paged.push(graph.push(id, &ids(parents)));
            }
        }

        assert_eq!(paged, whole);
    }
}
