//! Starknet utilises a custom Binary Merkle-Patricia Tree to store and organise its state.
//!
//! From an external perspective the tree is similar to a key-value store, where both key
//! and value are [Felts](FieldElement). The difference is that each tree is immutable,
//! and any mutations result in a new tree with a new root. This mutated variant can then
//! be accessed via the new root, and the old variant via the old root.
//!
//! Trees share common nodes to be efficient. These nodes perform reference counting and
//! will get deleted once all references are gone. State can therefore be tracked over time
//! by mutating the current state, and storing the new root. Old states can be dropped by
//! deleting old roots which are no longer required.
//!
//! #### Tree definition
//!
//! It is important to understand that since all keys are [Felts](FieldElement), this means
//! all paths to a key are equally long - 251 bits.
//!
//! Starknet defines three node types for a tree.
//!
//! `Leaf nodes` which represent an actual value stored.
//!
//! `Edge nodes` which connect two nodes, and __must be__ a maximal subtree (i.e. be as
//! long as possible). This latter condition is important as it strictly defines a tree (i.e. all
//! trees with the same leaves must have the same nodes). The path of an edge node can therefore
//! be many bits long.
//!
//! `Binary nodes` is a branch node with two children, left and right. This represents
//! only a single bit on the path to a leaf.
//!
//! A tree storing a single key-value would consist of two nodes. The root node would be an edge
//! node with a path equal to the key. This edge node is connected to a leaf node storing the value.
//!
//! #### Implementation details
//!
//! We've defined an additional node type, an `Unresolved node`. This is used to
//! represent a node who's hash is known, but has not yet been retrieved from storage (and we
//! therefore have no further details about it).
//!
//! Our implementation is a mix of nodes from persistent storage and any mutations are kept
//! in-memory. It is done this way to allow many mutations to a tree before committing only the
//! final result to storage. This may be confusing since we just said trees are immutable -- but
//! since we are only changing the in-memory tree, the immutable tree still exists in storage. One
//! can therefore think of the in-memory tree as containing the state changes between tree `N` and
//! `N + 1`.
//!
//! The in-memory tree is built using a graph of `Rc<RefCell<Node>>` which is a bit painful.

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::iter::once;
use core::marker::PhantomData;
use core::ops::ControlFlow;

use bitvec::prelude::{BitSlice, BitVec, Msb0};
use starknet_crypto::FieldElement;

use crate::crypto::merkle_patricia_tree::merkle_node::{BinaryNode, Direction, EdgeNode, Node};
use crate::traits::hash::CryptoHasher;

/// Lightweight representation of [BinaryNode]. Only holds left and right hashes.
#[derive(Debug, PartialEq, Eq)]
pub struct BinaryProofNode {
    /// Left hash.
    pub left_hash: FieldElement,
    /// Right hash.
    pub right_hash: FieldElement,
}

impl From<&BinaryNode> for ProofNode {
    fn from(bin: &BinaryNode) -> Self {
        Self::Binary(BinaryProofNode {
            left_hash: bin.left.borrow().hash().expect("Node should be committed"),
            right_hash: bin.right.borrow().hash().expect("Node should be committed"),
        })
    }
}

/// Ligthtweight representation of [EdgeNode]. Only holds its path and its child's hash.
#[derive(Debug, PartialEq, Eq)]
pub struct EdgeProofNode {
    /// Path of the node.
    pub path: BitVec<Msb0, u8>,
    /// Hash of the child node.
    pub child_hash: FieldElement,
}

impl From<&EdgeNode> for ProofNode {
    fn from(edge: &EdgeNode) -> Self {
        Self::Edge(EdgeProofNode {
            path: edge.path.clone(),
            child_hash: edge.child.borrow().hash().expect("Node should be committed"),
        })
    }
}
/// A binary node which can be read / written from an [RcNodeStorage].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedBinaryNode {
    /// Left node that's saved into db (soon gone I guess).
    pub left: FieldElement,
    /// Right node that's saved into db (soon gone I guess).
    pub right: FieldElement,
}

/// An edge node which can be read / written from an [RcNodeStorage].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedEdgeNode {
    /// Path of the node that's saved into db (soon gone I guess).
    pub path: BitVec<Msb0, u8>,
    /// Hash of the child node that's saved into db (soon gone I guess).
    pub child: FieldElement,
}

/// A node which can be read / written from an [RcNodeStorage].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistedNode {
    /// Persistent binary node.
    Binary(PersistedBinaryNode),
    /// Persistent edge node.
    Edge(PersistedEdgeNode),
    /// Persistent leaf.
    Leaf,
}

/// [ProofNode] s are lightweight versions of their `Node` counterpart.
/// They only consist of [BinaryProofNode] and [EdgeProofNode] because `Leaf`
/// and `Unresolved` nodes should not appear in a proof.
#[derive(Debug, PartialEq, Eq)]
pub enum ProofNode {
    /// Binary node.
    Binary(BinaryProofNode),
    /// Edge node.
    Edge(EdgeProofNode),
}

/// A Starknet binary Merkle-Patricia tree with a specific root entry-point and storage.
///
/// This is used to update, mutate and access global Starknet state as well as individual contract
/// states.
///
/// For more information on how this functions internally, see [here](super::merkle_tree).
#[derive(Debug, Clone)]
pub struct MerkleTree<T, H: CryptoHasher> {
    storage: T,
    root: Rc<RefCell<Node>>,
    max_height: u8,
    _hasher: PhantomData<H>,
}
/// Backing storage for Starknet Binary Merkle Patricia Tree.
///
/// Default implementation and persistent implementation is the `RcNodeStorage`. Testing/future
/// implementations include [`HashMap`](std::collections::HashMap) and `()` based implementations
/// where the backing storage is not persistent, or doesn't exist at all. The nodes will still be
/// visitable in-memory.
pub trait NodeStorage {
    /// Find a persistent node during a traversal from the storage.
    fn get(&self, key: FieldElement) -> Option<PersistedNode>;

    /// Insert or ignore if already exists `node` to storage under the given `key`.
    ///
    /// This does not imply incrementing the nodes ref count.
    fn upsert(&self, key: FieldElement, node: PersistedNode);

    /// Decrement previously stored `key`'s reference count. This shouldn't fail for key not found.
    #[cfg(any(feature = "test-utils", test))]
    fn decrement_ref_count(&self, key: FieldElement);

    /// Increment previously stored `key`'s reference count. This shouldn't fail for key not found.
    fn increment_ref_count(&self, key: FieldElement);
}

impl NodeStorage for () {
    fn get(&self, _key: FieldElement) -> Option<PersistedNode> {
        // the rc<refcell> impl will do just fine by without any backing for transaction tree
        // building
        None
    }

    fn upsert(&self, _key: FieldElement, _node: PersistedNode) {}

    #[cfg(any(feature = "test-utils", test))]
    fn decrement_ref_count(&self, _key: FieldElement) {}

    fn increment_ref_count(&self, _key: FieldElement) {}
}

impl<T: NodeStorage, H: CryptoHasher> MerkleTree<T, H> {
    /// Less visible initialization for `MerkleTree<T>` as the main entry points should be
    /// [`MerkleTree::<RcNodeStorage>::load`] for persistent trees and [`MerkleTree::empty`] for
    /// transient ones.
    fn new(storage: T, root: FieldElement, max_height: u8) -> Self {
        let root_node = Rc::new(RefCell::new(Node::Unresolved(root)));
        Self { storage, root: root_node, max_height, _hasher: PhantomData }
    }

    /// Empty tree.
    pub fn empty(storage: T, max_height: u8) -> Self {
        Self::new(storage, FieldElement::ZERO, max_height)
    }

    /// Persists all changes to storage and returns the new root hash.
    ///
    /// Note that the root is reference counted in storage. Committing the
    /// same tree again will therefore increment the count again.
    pub fn commit(mut self) -> FieldElement {
        self.commit_mut()
    }
    /// Return the state root.
    pub fn commit_mut(&mut self) -> FieldElement {
        // Go through tree, collect dirty nodes, calculate their hashes and
        // persist them. Take care to increment ref counts of child nodes. So in order
        // to do this correctly, will have to start back-to-front.
        self.commit_subtree(&mut self.root.borrow_mut());
        // unwrap is safe as `commit_subtree` will set the hash.
        let root = self.root.borrow().hash().unwrap();
        self.storage.increment_ref_count(root);

        // TODO: (debug only) expand tree assert that no edge node has edge node as child

        root
    }

    /// Persists any changes in this subtree to storage.
    ///
    /// This necessitates recursively calculating the hash of, and
    /// in turn persisting, any changed child nodes. This is necessary
    /// as the parent node's hash relies on its childrens hashes.
    ///
    /// In effect, the entire subtree gets persisted.
    fn commit_subtree(&self, node: &mut Node) {
        use Node::*;
        match node {
            Unresolved(_) => { /* Unresolved nodes are already persisted. */ }
            Leaf(_) => { /* storage wouldn't persist these even if we asked. */ }
            Binary(binary) if binary.hash.is_some() => { /* not dirty, already persisted */ }
            Edge(edge) if edge.hash.is_some() => { /* not dirty, already persisted */ }

            Binary(binary) => {
                self.commit_subtree(&mut binary.left.borrow_mut());
                self.commit_subtree(&mut binary.right.borrow_mut());
                // This will succeed as `commit_subtree` will set the child hashes.
                binary.calculate_hash::<H>();
                // unwrap is safe as `commit_subtree` will set the hashes.
                let left = binary.left.borrow().hash().unwrap();
                let right = binary.right.borrow().hash().unwrap();
                let persisted_node = PersistedNode::Binary(PersistedBinaryNode { left, right });
                // unwrap is safe as we just set the hash.
                self.storage.upsert(binary.hash.unwrap(), persisted_node);
            }

            Edge(edge) => {
                self.commit_subtree(&mut edge.child.borrow_mut());
                // This will succeed as `commit_subtree` will set the child's hash.
                edge.calculate_hash::<H>();

                // unwrap is safe as `commit_subtree` will set the hash.
                let child = edge.child.borrow().hash().unwrap();
                let persisted_node = PersistedNode::Edge(PersistedEdgeNode { path: edge.path.clone(), child });
                // unwrap is safe as we just set the hash.
                self.storage.upsert(edge.hash.unwrap(), persisted_node);
            }
        }
    }

    /// Sets the value of a key. To delete a key, set the value to [FieldElement::ZERO].
    pub fn set(&mut self, key: &BitSlice<Msb0, u8>, value: FieldElement) {
        if value == FieldElement::ZERO {
            return self.delete_leaf(key);
        }

        // Changing or inserting a new leaf into the tree will change the hashes
        // of all nodes along the path to the leaf.
        let path = self.traverse(key);
        for node in &path {
            node.borrow_mut().mark_dirty();
        }

        // There are three possibilities.
        //
        // 1. The leaf exists, in which case we simply change its value.
        //
        // 2. The tree is empty, we insert the new leaf and the root becomes an edge node connecting to it.
        //
        // 3. The leaf does not exist, and the tree is not empty. The final node in the traversal will
        //    be an edge node who's path diverges from our new leaf node's.
        //
        //    This edge must be split into a new subtree containing both the existing edge's child and the
        //    new leaf. This requires an edge followed by a binary node and then further edges to both the
        //    current child and the new leaf. Any of these new edges may also end with an empty path in
        //    which case they should be elided. It depends on the common path length of the current edge
        //    and the new leaf i.e. the split may be at the first bit (in which case there is no leading
        //    edge), or the split may be in the middle (requires both leading and post edges), or the
        //    split may be the final bit (no post edge).
        use Node::*;
        match path.last() {
            Some(node) => {
                let updated = match &*node.borrow() {
                    Edge(edge) => {
                        let common = edge.common_path(key);

                        // Height of the binary node
                        let branch_height = edge.height + common.len();
                        // Height of the binary node's children
                        let child_height = branch_height + 1;

                        // Path from binary node to new leaf
                        let new_path = key[child_height..].to_vec();
                        // Path from binary node to existing child
                        let old_path = edge.path[common.len() + 1..].to_vec();

                        // The new leaf branch of the binary node.
                        // (this may be edge -> leaf, or just leaf depending).
                        let new_leaf = Node::Leaf(value);
                        let new = match new_path.is_empty() {
                            true => Rc::new(RefCell::new(new_leaf)),
                            false => {
                                let new_edge = Node::Edge(EdgeNode {
                                    hash: None,
                                    height: child_height,
                                    path: new_path,
                                    child: Rc::new(RefCell::new(new_leaf)),
                                });
                                Rc::new(RefCell::new(new_edge))
                            }
                        };

                        // The existing child branch of the binary node.
                        let old = match old_path.is_empty() {
                            true => edge.child.clone(),
                            false => {
                                let old_edge = Node::Edge(EdgeNode {
                                    hash: None,
                                    height: child_height,
                                    path: old_path,
                                    child: edge.child.clone(),
                                });
                                Rc::new(RefCell::new(old_edge))
                            }
                        };

                        let new_direction = Direction::from(key[branch_height]);
                        let (left, right) = match new_direction {
                            Direction::Left => (new, old),
                            Direction::Right => (old, new),
                        };

                        let branch = Node::Binary(BinaryNode { hash: None, height: branch_height, left, right });

                        // We may require an edge leading to the binary node.
                        match common.is_empty() {
                            true => branch,
                            false => Node::Edge(EdgeNode {
                                hash: None,
                                height: edge.height,
                                path: common.to_vec(),
                                child: Rc::new(RefCell::new(branch)),
                            }),
                        }
                    }
                    // Leaf exists, we replace its value.
                    Leaf(_) => Node::Leaf(value),
                    Unresolved(_) | Binary(_) => {
                        unreachable!("The end of a traversion cannot be unresolved or binary")
                    }
                };

                node.swap(&RefCell::new(updated));
            }
            None => {
                // Getting no travel nodes implies that the tree is empty.
                //
                // Create a new leaf node with the value, and the root becomes
                // an edge node connecting to the leaf.
                let leaf = Node::Leaf(value);
                let edge = Node::Edge(EdgeNode {
                    hash: None,
                    height: 0,
                    path: key.to_vec(),
                    child: Rc::new(RefCell::new(leaf)),
                });

                self.root = Rc::new(RefCell::new(edge));
            }
        }
    }

    /// Deletes a leaf node from the tree.
    ///
    /// This is not an external facing API; the functionality is instead accessed by calling
    /// [`MerkleTree::set`] with value set to [`FieldElement::ZERO`].
    fn delete_leaf(&mut self, key: &BitSlice<Msb0, u8>) {
        // Algorithm explanation:
        //
        // The leaf's parent node is either an edge, or a binary node.
        // If it's an edge node, then it must also be deleted. And its parent
        // must be a binary node. In either case we end up with a binary node
        // who's one child is deleted. This changes the binary to an edge node.
        //
        // Note that its possible that there is no binary node -- if the resulting tree would be empty.
        //
        // This new edge node may need to merge with the old binary node's parent node
        // and other remaining child node -- if they're also edges.
        //
        // Then we are done.
        let path = self.traverse(key);

        // Do nothing if the leaf does not exist.
        match path.last() {
            Some(node) => match &*node.borrow() {
                Node::Leaf(_) => {}
                _ => return,
            },
            None => return,
        }

        // All hashes along the path will become invalid (if they aren't deleted).
        for node in &path {
            node.borrow_mut().mark_dirty();
        }

        // Go backwards until we hit a branch node.
        let mut node_iter = path.into_iter().rev().skip_while(|node| !node.borrow().is_binary());

        match node_iter.next() {
            Some(node) => {
                let new_edge = {
                    // This node must be a binary node due to the iteration condition.
                    let binary = node.borrow().as_binary().cloned().unwrap();
                    // Create an edge node to replace the old binary node
                    // i.e. with the remaining child (note the direction invert),
                    //      and a path of just a single bit.
                    let direction = binary.direction(key).invert();
                    let child = binary.get_child(direction);
                    let path = once(bool::from(direction)).collect::<BitVec<_, _>>();
                    let mut edge = EdgeNode { hash: None, height: binary.height, path, child };

                    // Merge the remaining child if it's an edge.
                    self.merge_edges(&mut edge);

                    edge
                };
                // Replace the old binary node with the new edge node.
                node.swap(&RefCell::new(Node::Edge(new_edge)));
            }
            None => {
                // We reached the root without a hitting binary node. The new tree
                // must therefore be empty.
                self.root = Rc::new(RefCell::new(Node::Unresolved(FieldElement::ZERO)));
                return;
            }
        };

        // Check the parent of the new edge. If it is also an edge, then they must merge.
        if let Some(node) = node_iter.next() {
            if let Node::Edge(edge) = &mut *node.borrow_mut() {
                self.merge_edges(edge);
            }
        }
    }

    /// Returns the value stored at key, or `None` if it does not exist.
    pub fn get(&self, key: &BitSlice<Msb0, u8>) -> Option<FieldElement> {
        self.traverse(key).last().and_then(|node| match &*node.borrow() {
            Node::Leaf(value) if !value.eq(&FieldElement::ZERO) => Some(*value),
            _ => None,
        })
    }

    /// Generates a merkle-proof for a given `key`.
    ///
    /// Returns vector of [`ProofNode`] which form a chain from the root to the key,
    /// if it exists, or down to the node which proves that the key does not exist.
    ///
    /// The nodes are returned in order, root first.
    ///
    /// Verification is performed by confirming that:
    ///   1. the chain follows the path of `key`, and
    ///   2. the hashes are correct, and
    ///   3. the root hash matches the known root
    pub fn get_proof(&self, key: &BitSlice<Msb0, u8>) -> Vec<ProofNode> {
        let mut nodes = self.traverse(key);

        // Return an empty list if tree is empty.
        let node = match nodes.last() {
            Some(node) => node,
            None => return Vec::new(),
        };

        // A leaf node is redudant data as the information for it is already contained in the previous node.
        if matches!(&*node.borrow(), Node::Leaf(_)) {
            nodes.pop();
        }

        nodes
            .iter()
            .map(|node| match &*node.borrow() {
                Node::Binary(bin) => ProofNode::from(bin),
                Node::Edge(edge) => ProofNode::from(edge),
                _ => unreachable!(),
            })
            .collect()
    }

    /// Traverses from the current root towards the destination [Leaf](Node::Leaf) node.
    /// Returns the list of nodes along the path.
    ///
    /// If the destination node exists, it will be the final node in the list.
    ///
    /// This means that the final node will always be either a the destination [Leaf](Node::Leaf)
    /// node, or an [Edge](Node::Edge) node who's path suffix does not match the leaf's path.
    ///
    /// The final node can __not__ be a [Binary](Node::Binary) node since it would always be
    /// possible to continue on towards the destination. Nor can it be an
    /// [Unresolved](Node::Unresolved) node since this would be resolved to check if we can
    /// travel further.
    fn traverse(&self, dst: &BitSlice<Msb0, u8>) -> Vec<Rc<RefCell<Node>>> {
        if self.root.borrow().is_empty() {
            return Vec::new();
        }

        let mut current = self.root.clone();
        let mut height = 0;
        let mut nodes = Vec::new();
        loop {
            use Node::*;

            let current_tmp = current.borrow().clone();

            let next = match current_tmp {
                Unresolved(hash) => {
                    let node = self.resolve(hash, height);
                    current.swap(&RefCell::new(node));
                    current
                }
                Binary(binary) => {
                    nodes.push(current.clone());
                    let next = binary.direction(dst);
                    let next = binary.get_child(next);
                    height += 1;
                    next
                }
                Edge(edge) if edge.path_matches(dst) => {
                    nodes.push(current.clone());
                    height += edge.path.len();
                    edge.child.clone()
                }
                Leaf(_) | Edge(_) => {
                    nodes.push(current);
                    return nodes;
                }
            };

            current = next;
        }
    }

    /// Retrieves the requested node from storage.
    ///
    /// Result will be either a [Binary](Node::Binary), [Edge](Node::Edge) or [Leaf](Node::Leaf)
    /// node.
    fn resolve(&self, hash: FieldElement, height: usize) -> Node {
        if height == self.max_height as usize {
            #[cfg(debug_assertions)]
            match self.storage.get(hash) {
                Some(PersistedNode::Edge(_) | PersistedNode::Binary(_)) | None => {
                    // some cases are because of collisions, none is the common outcome
                }
                Some(PersistedNode::Leaf) => {
                    // they exist in some databases, but in general we run only release builds
                    // against real databases
                    unreachable!("leaf nodes should no longer exist");
                }
            }
            return Node::Leaf(hash);
        }

        match self.storage.get(hash).unwrap() {
            PersistedNode::Binary(binary) => Node::Binary(BinaryNode {
                hash: Some(hash),
                height,
                left: Rc::new(RefCell::new(Node::Unresolved(binary.left))),
                right: Rc::new(RefCell::new(Node::Unresolved(binary.right))),
            }),
            PersistedNode::Edge(edge) => Node::Edge(EdgeNode {
                hash: Some(hash),
                height,
                path: edge.path,
                child: Rc::new(RefCell::new(Node::Unresolved(edge.child))),
            }),
            PersistedNode::Leaf => {
                panic!("Retrieved node {hash} is a leaf at {height} out of {}", self.max_height)
            }
        }
    }

    /// This is a convenience function which merges the edge node with its child __iff__ it is also
    /// an edge.
    ///
    /// Does nothing if the child is not also an edge node.
    ///
    /// This can occur when mutating the tree (e.g. deleting a child of a binary node), and is an
    /// illegal state (since edge nodes __must be__ maximal subtrees).
    fn merge_edges(&self, parent: &mut EdgeNode) {
        let resolved_child = match &*parent.child.borrow() {
            Node::Unresolved(hash) => self.resolve(*hash, parent.height + parent.path.len()),
            other => other.clone(),
        };

        if let Some(child_edge) = resolved_child.as_edge().cloned() {
            parent.path.extend_from_slice(&child_edge.path);
            parent.child = child_edge.child;
        }
    }

    /// Visits all of the nodes in the tree in pre-order using the given visitor function.
    ///
    /// For each node, there will first be a visit for `Node::Unresolved(hash)` followed by visit
    /// at the loaded node when [`Visit::ContinueDeeper`] is returned. At any time the visitor
    /// function can also return `ControlFlow::Break` to stop the visit with the given return
    /// value, which will be returned as `Some(value))` to the caller.
    ///
    /// The visitor function receives the node being visited, as well as the full path to that node.
    ///
    /// Upon successful non-breaking visit of the tree, `None` will be returned.
    #[allow(dead_code)]
    pub fn dfs<X, VisitorFn>(&self, visitor_fn: &mut VisitorFn) -> Option<X>
    where
        VisitorFn: FnMut(&Node, &BitSlice<Msb0, u8>) -> ControlFlow<X, Visit>,
    {
        use bitvec::prelude::bitvec;

        #[allow(dead_code)]
        struct VisitedNode {
            node: Rc<RefCell<Node>>,
            path: BitVec<Msb0, u8>,
        }

        let mut visiting = vec![VisitedNode { node: self.root.clone(), path: bitvec![Msb0, u8;] }];

        loop {
            match visiting.pop() {
                None => break,
                Some(VisitedNode { node, path }) => {
                    let current_node = &*node.borrow();
                    let _zero = FieldElement::from(0_u32);
                    if !matches!(current_node, Node::Unresolved(_zero)) {
                        match visitor_fn(current_node, &path) {
                            ControlFlow::Continue(Visit::ContinueDeeper) => {
                                // the default, no action, just continue deeper
                            }
                            ControlFlow::Continue(Visit::StopSubtree) => {
                                // make sure we don't add any more to `visiting` on this subtree
                                continue;
                            }
                            ControlFlow::Break(x) => {
                                // early exit
                                return Some(x);
                            }
                        }
                    }
                    match current_node {
                        Node::Binary(b) => {
                            visiting.push(VisitedNode {
                                node: b.right.clone(),
                                path: {
                                    let mut path_right = path.clone();
                                    path_right.push(Direction::Right.into());
                                    path_right
                                },
                            });
                            visiting.push(VisitedNode {
                                node: b.left.clone(),
                                path: {
                                    let mut path_left = path.clone();
                                    path_left.push(Direction::Left.into());
                                    path_left
                                },
                            });
                        }
                        Node::Edge(e) => {
                            visiting.push(VisitedNode {
                                node: e.child.clone(),
                                path: {
                                    let mut extended_path = path.clone();
                                    extended_path.extend_from_slice(&e.path);
                                    extended_path
                                },
                            });
                        }
                        Node::Leaf(_) => {}
                        Node::Unresolved(hash) => {
                            // Zero means empty tree, so nothing to resolve
                            if hash != &FieldElement::ZERO {
                                visiting.push(VisitedNode {
                                    node: Rc::new(RefCell::new(self.resolve(*hash, path.len()))),
                                    path,
                                });
                            }
                        }
                    };
                }
            }
        }

        None
    }
}

/// Direction for the [`MerkleTree::dfs`] as the return value of the visitor function.
#[derive(Default)]
pub enum Visit {
    /// Instructs that the visit should visit any subtrees of the current node. This is a no-op for
    /// [`Node::Leaf`].
    #[default]
    ContinueDeeper,
    /// Returning this value for [`Node::Binary`] or [`Node::Edge`] will ignore all of the children
    /// of the node for the rest of the iteration. This is useful because two trees often share a
    /// number of subtrees with earlier blocks. Returning this for [`Node::Leaf`] is a no-op.
    StopSubtree,
}