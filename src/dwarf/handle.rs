use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;
use super::DwarfParser;

/// Async handle to a DWARF parse result that may still be in progress.
///
/// Uses `tokio::sync::watch` internally — cloneable, retains last value,
/// async-awaitable. The `Option` distinguishes "pending" from "complete".
/// Error is `String` because `crate::Error` isn't `Clone`.
#[derive(Clone)]
pub struct DwarfHandle {
    rx: watch::Receiver<Option<Result<Arc<DwarfParser>, String>>>,
}

impl DwarfHandle {
    /// Start a background DWARF parse via `spawn_blocking`. Returns immediately.
    /// If `search_root` is provided, it will be searched for .dSYM bundles when
    /// the binary doesn't have embedded DWARF (common on macOS).
    /// If `symbols_path` is provided, it will be tried first (explicit dSYM or DWARF file).
    pub fn spawn_parse(binary_path: &str, search_root: Option<&str>, symbols_path: Option<&str>) -> Self {
        let (tx, rx) = watch::channel(None);
        let path = binary_path.to_string();
        let root = search_root.map(|s| s.to_string());
        let sym_path = symbols_path.map(|s| s.to_string());

        tokio::task::spawn_blocking(move || {
            let result = DwarfParser::parse_with_options(
                    Path::new(&path),
                    root.as_deref().map(Path::new),
                    sym_path.as_deref().map(Path::new),
                )
                .map(Arc::new)
                .map_err(|e| e.to_string());
            let _ = tx.send(Some(result));
        });

        Self { rx }
    }

    /// Check if this handle resolved to a failed parse.
    /// Returns false if still pending or if parse succeeded.
    pub fn is_failed(&self) -> bool {
        matches!(self.rx.borrow().as_ref(), Some(Err(_)))
    }

    /// Wrap an already-parsed result (cache hit).
    pub fn ready(dwarf: Arc<DwarfParser>) -> Self {
        let (_, rx) = watch::channel(Some(Ok(dwarf)));
        Self { rx }
    }

    /// Await parse completion and return the result.
    pub async fn get(&mut self) -> crate::Result<Arc<DwarfParser>> {
        // Wait until the value is Some (parse complete)
        self.rx
            .wait_for(|v| v.is_some())
            .await
            .map_err(|_| crate::Error::Frida("DWARF parse task was dropped".to_string()))?;

        self.rx
            .borrow()
            .as_ref()
            .unwrap()
            .clone()
            .map_err(|e| crate::Error::Frida(e))
    }

    /// Try to synchronously borrow the parsed result.
    /// Returns None if parse is still pending, Some(Ok(...)) if successful, Some(Err(...)) if failed.
    pub fn try_borrow_parser(&self) -> Option<Result<Arc<DwarfParser>, String>> {
        self.rx.borrow().as_ref().map(|r| r.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_parser() -> Arc<DwarfParser> {
        // DwarfParser doesn't have a public constructor, but we can use parse
        // on a path that won't have debug info. Instead, test with ready().
        // We'll test the channel mechanics.
        Arc::new(DwarfParser {
            functions: vec![],
            functions_by_name: std::collections::HashMap::new(),
            functions_by_addr: vec![],
            variables: vec![],
            variables_by_name: std::collections::HashMap::new(),
            struct_members: std::sync::Mutex::new(std::collections::HashMap::new()),
            lazy_struct_info: std::collections::HashMap::new(),
            line_table: std::sync::Mutex::new(None),
            image_base: 0x100000,
            binary_path: None,
        })
    }

    #[tokio::test]
    async fn test_ready_returns_immediately() {
        let parser = make_parser();
        let mut handle = DwarfHandle::ready(parser.clone());
        let result = handle.get().await.unwrap();
        assert_eq!(result.image_base, 0x100000);
    }

    #[tokio::test]
    async fn test_clone_both_resolve() {
        let parser = make_parser();
        let handle = DwarfHandle::ready(parser.clone());
        let mut h1 = handle.clone();
        let mut h2 = handle.clone();
        let r1 = h1.get().await.unwrap();
        let r2 = h2.get().await.unwrap();
        assert_eq!(r1.image_base, r2.image_base);
    }
}
