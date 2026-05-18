use lattice_core::VaultPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GraphNodeId(pub uuid::Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphNodeKind {
    Note,
    Heading,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNode {
    pub id: GraphNodeId,
    pub label: String,
    pub path: Option<VaultPath>,
    pub kind: GraphNodeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphEdgeKind {
    Wikilink,
    MarkdownLink,
    Tag,
    HeadingLink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdge {
    pub from: GraphNodeId,
    pub to: GraphNodeId,
    pub kind: GraphEdgeKind,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GraphSnapshot {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}
