//! Primary statement counters and separately attributed forward-fetch target work.
use crate::QueryResult;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct QueryMetrics {
    pub rows_read: u64,
    pub rows_written: u64,
    pub fullscan_steps: u64,
    pub index_steps: u64,
    pub vm_steps: u64,
    pub sort_operations: u64,
    pub btree_seeks: u64,
    pub fetch_batches: u64,
    pub fetch_rows_read: u64,
    pub fetch_vm_steps: u64,
    /// Adjacency index entries read by GRAPH sources, including rejected candidates.
    pub graph_adjacency_entries: u64,
    /// Candidate (path or node, edge) expansions considered by GRAPH sources.
    pub graph_expansions: u64,
    /// Edge or node documents loaded to evaluate GRAPH step predicates.
    pub graph_documents: u64,
    /// Rows materialized by GRAPH sources before outer SQL filtering.
    pub graph_rows: u64,
    /// Nodes marked visited (REACHABLE) or expanded as path ends (PATHS).
    pub graph_visited: u64,
    /// Peak bytes of graph-owned traversal state, as conservatively estimated.
    pub graph_peak_bytes: u64,
    /// Index key ranges searched by spatial sources and rewrites, in all rounds.
    pub spatial_ranges: u64,
    /// Radius rounds run by nearest searches before the final statement.
    pub spatial_rounds: u64,
}
impl QueryMetrics {
    pub(crate) fn from_statement(statement: &turso_core::Statement) -> Self {
        let metrics = statement.metrics();
        Self {
            rows_read: metrics.rows_read,
            rows_written: metrics.rows_written,
            fullscan_steps: metrics.fullscan_steps,
            index_steps: metrics.index_steps,
            vm_steps: metrics.insn_executed,
            sort_operations: metrics.sort_operations,
            btree_seeks: metrics.btree_seeks,
            fetch_batches: 0,
            fetch_rows_read: 0,
            fetch_vm_steps: 0,
            graph_adjacency_entries: 0,
            graph_expansions: 0,
            graph_documents: 0,
            graph_rows: 0,
            graph_visited: 0,
            graph_peak_bytes: 0,
            spatial_ranges: 0,
            spatial_rounds: 0,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct ProfiledQuery {
    pub result: QueryResult,
    pub metrics: QueryMetrics,
}
