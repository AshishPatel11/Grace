//! Split panes: a small binary tree. Each split has two children and a ratio.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x + d, self.y + d, (self.w - 2.0 * d).max(0.0), (self.h - 2.0 * d).max(0.0))
    }
    pub fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
    pub fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
}

#[derive(Clone, Debug)]
pub enum Node {
    Leaf(u64),
    /// `vertical` = the two children sit side by side (the dividing line is vertical).
    Split { vertical: bool, ratio: f32, a: Box<Node>, b: Box<Node> },
}

/// A divider the user can drag. `path` says how to reach its split from the root.
#[derive(Clone, Debug)]
pub struct Divider {
    pub rect: Rect,
    pub vertical: bool,
    pub path: Vec<bool>,
    pub parent: Rect,
}

impl Node {
    pub fn leaves(&self) -> Vec<u64> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<u64>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    pub fn layout(&self, r: Rect, gap: f32, panes: &mut Vec<(u64, Rect)>, divs: &mut Vec<Divider>) {
        let mut path = Vec::new();
        self.layout_inner(r, gap, panes, divs, &mut path);
    }

    fn layout_inner(&self, r: Rect, gap: f32, panes: &mut Vec<(u64, Rect)>, divs: &mut Vec<Divider>, path: &mut Vec<bool>) {
        match self {
            Node::Leaf(id) => panes.push((*id, r)),
            Node::Split { vertical, ratio, a, b } => {
                let (ra, rb, d) = if *vertical {
                    let wa = ((r.w - gap) * ratio).round();
                    let ra = Rect::new(r.x, r.y, wa, r.h);
                    let rb = Rect::new(r.x + wa + gap, r.y, (r.w - wa - gap).max(0.0), r.h);
                    (ra, rb, Rect::new(r.x + wa, r.y, gap, r.h))
                } else {
                    let ha = ((r.h - gap) * ratio).round();
                    let ra = Rect::new(r.x, r.y, r.w, ha);
                    let rb = Rect::new(r.x, r.y + ha + gap, r.w, (r.h - ha - gap).max(0.0));
                    (ra, rb, Rect::new(r.x, r.y + ha, r.w, gap))
                };
                divs.push(Divider { rect: d, vertical: *vertical, path: path.clone(), parent: r });
                path.push(false);
                a.layout_inner(ra, gap, panes, divs, path);
                path.pop();
                path.push(true);
                b.layout_inner(rb, gap, panes, divs, path);
                path.pop();
            }
        }
    }

    /// Put `new_id` next to `target`. Returns false if `target` is not in this tree.
    pub fn split(&mut self, target: u64, new_id: u64, vertical: bool) -> bool {
        self.split_with(target, Node::Leaf(new_id), vertical)
    }

    /// Put a whole layout (another tab's panes) next to `target`: right of it when
    /// `vertical`, below it otherwise. Returns false if `target` is not in this tree.
    pub fn split_with(&mut self, target: u64, node: Node, vertical: bool) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                *self = Node::Split { vertical, ratio: 0.5, a: Box::new(Node::Leaf(target)), b: Box::new(node) };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => {
                // Try `a` first without giving `node` away; only one side can hold `target`.
                if a.leaves().contains(&target) {
                    a.split_with(target, node, vertical)
                } else {
                    b.split_with(target, node, vertical)
                }
            }
        }
    }

    /// Remove a leaf. Its sibling takes the parent's place.
    /// Returns false when `target` is the root leaf (the caller closes the tab).
    pub fn remove(&mut self, target: u64) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => {
                if matches!(**a, Node::Leaf(id) if id == target) {
                    *self = (**b).clone();
                    return true;
                }
                if matches!(**b, Node::Leaf(id) if id == target) {
                    *self = (**a).clone();
                    return true;
                }
                a.remove(target) || b.remove(target)
            }
        }
    }

    pub fn set_ratio(&mut self, path: &[bool], ratio: f32) {
        match self {
            Node::Leaf(_) => {}
            Node::Split { ratio: r, a, b, .. } => {
                if path.is_empty() {
                    *r = ratio.clamp(0.1, 0.9);
                } else if path[0] {
                    b.set_ratio(&path[1..], ratio)
                } else {
                    a.set_ratio(&path[1..], ratio)
                }
            }
        }
    }

    pub fn replace(&mut self, old: u64, new: u64) {
        match self {
            Node::Leaf(id) => {
                if *id == old {
                    *id = new
                }
            }
            Node::Split { a, b, .. } => {
                a.replace(old, new);
                b.replace(old, new);
            }
        }
    }
}

/// Pick the pane next to `from` in a direction (dx, dy are -1/0/1).
pub fn neighbor(rects: &[(u64, Rect)], from: u64, dx: i32, dy: i32) -> Option<u64> {
    let cur = rects.iter().find(|(id, _)| *id == from)?.1;
    let mut best: Option<(f32, u64)> = None;
    for (id, r) in rects {
        if *id == from {
            continue;
        }
        let ok = match (dx, dy) {
            (1, _) => r.x >= cur.right() - 1.0 && r.y < cur.bottom() && r.bottom() > cur.y,
            (-1, _) => r.right() <= cur.x + 1.0 && r.y < cur.bottom() && r.bottom() > cur.y,
            (_, 1) => r.y >= cur.bottom() - 1.0 && r.x < cur.right() && r.right() > cur.x,
            _ => r.bottom() <= cur.y + 1.0 && r.x < cur.right() && r.right() > cur.x,
        };
        if ok {
            let d = (r.cx() - cur.cx()).abs() + (r.cy() - cur.cy()).abs();
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, *id));
            }
        }
    }
    best.map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_tab_joins_next_to_a_pane() {
        // Tab A: panes 1 | 2. Tab B: panes 3 / 4. Put B right of pane 2.
        let mut a = Node::Leaf(1);
        a.split(1, 2, true);
        let mut b = Node::Leaf(3);
        b.split(3, 4, false);
        assert!(a.split_with(2, b, true));
        assert_eq!(a.leaves(), vec![1, 2, 3, 4]);
        // Pane 2 and the B layout now share one split, B on the right.
        match &a {
            Node::Split { b: right, .. } => match &**right {
                Node::Split { vertical, a: l, b: r, .. } => {
                    assert!(*vertical);
                    assert!(matches!(**l, Node::Leaf(2)));
                    assert_eq!(r.leaves(), vec![3, 4]);
                }
                _ => panic!("pane 2 was not split"),
            },
            _ => panic!("root is not a split"),
        }
        // A pane that is not there: nothing changes.
        assert!(!a.split_with(9, Node::Leaf(5), true));
        assert_eq!(a.leaves(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn a_joined_pane_can_leave_again() {
        let mut a = Node::Leaf(1);
        a.split_with(1, Node::Leaf(2), false);
        assert!(a.remove(2));
        assert_eq!(a.leaves(), vec![1]);
    }
}
