use crate::{error::GritResult, AstCursor, ByteRange, CodeRange};
use std::borrow::Cow;

/// Represents an AST node and offers convenient AST-specific functionality.
///
/// This trait should be free from dependencies on TreeSitter. This also implies
/// it should not expose details about the node that may make it infeasible to
/// implement the trait by implementations that use different node
/// representations internally.
pub trait AstNode: std::fmt::Debug + Sized {
    /// Returns an iterator over the node's ancestors, starting with the node
    /// itself and moving up to the root.
    fn ancestors(&self) -> impl Iterator<Item = Self>;

    /// Returns an iterator over the node's children.
    fn children(&self) -> impl Iterator<Item = Self>;

    /// Returns the node's parent.
    ///
    /// Returns `None` if this is the root node.
    fn parent(&self) -> Option<Self>;

    /// Returns the next node in the tree, ignoring trivia such as whitespace.
    fn next_named_node(&self) -> Option<Self>;

    /// Returns the previous node in the tree, ignoring trivia such as
    /// whitespace.
    fn previous_named_node(&self) -> Option<Self>;

    /// Returns the next adjacent node.
    fn next_sibling(&self) -> Option<Self>;

    /// Returns the previous adjacent node.
    fn previous_sibling(&self) -> Option<Self>;

    /// Returns the text representation of the node.
    fn text(&self) -> GritResult<Cow<str>>;

    /// Returns the byte range of the node.
    fn byte_range(&self) -> ByteRange;

    /// Returns the code range of the node.
    fn code_range(&self) -> CodeRange;

    /// Returns a cursor for traversing the tree, starting at the current node.
    fn walk(&self) -> impl AstCursor<Node = Self>;
}
