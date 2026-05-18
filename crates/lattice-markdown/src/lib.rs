use lattice_core::VaultPath;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wikilink {
    pub raw: String,
    pub target: String,
    pub alias: Option<String>,
    pub heading: Option<String>,
    pub byte_range: std::ops::Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    pub source: VaultPath,
    pub target: String,
    pub line: usize,
    pub snippet: String,
}

#[derive(Debug, Default)]
pub struct InMemoryMarkdownIndex {
    links_by_file: BTreeMap<VaultPath, Vec<Wikilink>>,
}

impl InMemoryMarkdownIndex {
    pub fn update_file(&mut self, path: VaultPath, contents: &str) {
        self.links_by_file.insert(path, extract_wikilinks(contents));
    }

    pub fn remove_file(&mut self, path: &VaultPath) {
        self.links_by_file.remove(path);
    }

    pub fn backlinks_for_target(&self, target: &str) -> Vec<Backlink> {
        self.links_by_file
            .iter()
            .flat_map(|(source, links)| {
                links.iter().filter_map(|link| {
                    (link.target == target).then(|| Backlink {
                        source: source.clone(),
                        target: target.to_owned(),
                        line: 0,
                        snippet: link.raw.clone(),
                    })
                })
            })
            .collect()
    }
}

pub fn extract_wikilinks(contents: &str) -> Vec<Wikilink> {
    let bytes = contents.as_bytes();
    let mut links = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if &bytes[i..i + 2] != b"[[" {
            i += 1;
            continue;
        }
        let start = i;
        i += 2;
        let inner_start = i;
        while i + 1 < bytes.len() && &bytes[i..i + 2] != b"]]" {
            i += 1;
        }
        if i + 1 >= bytes.len() {
            break;
        }
        let inner = &contents[inner_start..i];
        let raw = contents[start..i + 2].to_owned();
        let (target_part, alias) = inner
            .split_once('|')
            .map(|(target, alias)| (target, Some(alias.to_owned())))
            .unwrap_or((inner, None));
        let (target, heading) = target_part
            .split_once('#')
            .map(|(target, heading)| (target.to_owned(), Some(heading.to_owned())))
            .unwrap_or((target_part.to_owned(), None));
        links.push(Wikilink {
            raw,
            target,
            alias,
            heading,
            byte_range: start..i + 2,
        });
        i += 2;
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_alias_and_heading() {
        let links = extract_wikilinks("[[Note|Alias]] [[folder/Note#Heading]]");
        assert_eq!(links[0].target, "Note");
        assert_eq!(links[0].alias.as_deref(), Some("Alias"));
        assert_eq!(links[1].target, "folder/Note");
        assert_eq!(links[1].heading.as_deref(), Some("Heading"));
    }
}
