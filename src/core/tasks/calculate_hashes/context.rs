use super::*;

pub(crate) struct RepositoryHashContext {
    pub repository_url: String,
    pub tree: Tree,
}

impl RepositoryHashContext {
    pub(crate) async fn load(
        context: Arc<FoxyContext>,
        repository_url: &str,
    ) -> Option<RepositoryHashContext> {
        match Tree::load(context, repository_url).await {
            Ok(tree) => Some(RepositoryHashContext {
                repository_url: repository_url.to_owned(),
                tree,
            }),
            Err(err) => {
                warn!(
                    "Failed to load tree for incremental hash context {}: {}",
                    repository_url, err
                );
                None
            }
        }
    }
}
