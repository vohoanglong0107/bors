use std::sync::Arc;

use crate::bors::command::Approver;
use crate::bors::handlers::labels::handle_label_trigger;
use crate::bors::Comment;
use crate::bors::RepositoryState;
use crate::github::GithubUser;
use crate::github::LabelTrigger;
use crate::github::PullRequest;
use crate::permissions::PermissionType;
use crate::PgDbClient;

/// Approve a pull request.
/// A pull request can only be approved by a user of sufficient authority.
pub(super) async fn command_approve(
    repo_state: Arc<RepositoryState>,
    db: Arc<PgDbClient>,
    pr: &PullRequest,
    author: &GithubUser,
    approver: &Approver,
) -> anyhow::Result<()> {
    tracing::info!("Approving PR {}", pr.number);
    if !sufficient_approve_permission(repo_state.clone(), author) {
        deny_approve_request(&repo_state, pr, author).await?;
        return Ok(());
    };
    let approver = match approver {
        Approver::Myself => author.username.clone(),
        Approver::Specified(approver) => approver.clone(),
    };
    db.approve(repo_state.repository(), pr.number, approver.as_str())
        .await?;
    handle_label_trigger(&repo_state, pr.number, LabelTrigger::Approved).await?;
    notify_of_approval(&repo_state, pr, approver.as_str()).await
}

/// Unapprove a pull request.
/// Pull request's author can also unapprove the pull request.
pub(super) async fn command_unapprove(
    repo_state: Arc<RepositoryState>,
    db: Arc<PgDbClient>,
    pr: &PullRequest,
    author: &GithubUser,
) -> anyhow::Result<()> {
    tracing::info!("Unapproving PR {}", pr.number);
    if !sufficient_unapprove_permission(repo_state.clone(), pr, author) {
        deny_unapprove_request(&repo_state, pr, author).await?;
        return Ok(());
    };
    db.unapprove(repo_state.repository(), pr.number).await?;
    handle_label_trigger(&repo_state, pr.number, LabelTrigger::Unapproved).await?;
    notify_of_unapproval(&repo_state, pr).await
}

fn sufficient_approve_permission(repo: Arc<RepositoryState>, author: &GithubUser) -> bool {
    repo.permissions
        .load()
        .has_permission(author.id, PermissionType::Review)
}

async fn deny_approve_request(
    repo: &RepositoryState,
    pr: &PullRequest,
    author: &GithubUser,
) -> anyhow::Result<()> {
    tracing::warn!(
        "Permission denied for approve command by {}",
        author.username
    );
    repo.client
        .post_comment(
            pr.number,
            Comment::new(format!(
                "@{}: :key: Insufficient privileges: not in review users",
                author.username
            )),
        )
        .await
}

async fn notify_of_approval(
    repo: &RepositoryState,
    pr: &PullRequest,
    approver: &str,
) -> anyhow::Result<()> {
    repo.client
        .post_comment(
            pr.number,
            Comment::new(format!(
                "Commit {} has been approved by `{}`",
                pr.head.sha, approver
            )),
        )
        .await
}

fn sufficient_unapprove_permission(
    repo: Arc<RepositoryState>,
    pr: &PullRequest,
    author: &GithubUser,
) -> bool {
    author.id == pr.author.id
        || repo
            .permissions
            .load()
            .has_permission(author.id, PermissionType::Review)
}

async fn deny_unapprove_request(
    repo: &RepositoryState,
    pr: &PullRequest,
    author: &GithubUser,
) -> anyhow::Result<()> {
    tracing::warn!(
        "Permission denied for unapprove command by {}",
        author.username
    );
    repo.client
        .post_comment(
            pr.number,
            Comment::new(format!(
                "@{}: :key: Insufficient privileges: not in review users",
                author.username
            )),
        )
        .await
}

async fn notify_of_unapproval(repo: &RepositoryState, pr: &PullRequest) -> anyhow::Result<()> {
    repo.client
        .post_comment(
            pr.number,
            Comment::new(format!("Commit {} has been unapproved", pr.head.sha)),
        )
        .await
}

#[cfg(test)]
mod tests {
    use crate::{
        github::PullRequestNumber,
        tests::mocks::{
            default_pr_number, default_repo_name, BorsBuilder, BorsTester, Permissions, User, World,
        },
    };

    #[sqlx::test]
    async fn default_approve(pool: sqlx::PgPool) {
        let world = World::default();
        world.default_repo().lock().set_config(
            r#"
[labels]
approve = ["+approved"]
"#,
        );
        BorsBuilder::new(pool)
            .world(world)
            .run_test(|mut tester| async {
                tester.post_comment("@bors r+").await?;
                assert_eq!(
                    tester.get_comment().await?,
                    format!(
                        "Commit pr-{}-sha has been approved by `{}`",
                        default_pr_number(),
                        User::default_user().name
                    ),
                );

                check_pr_approved_by(
                    &tester,
                    default_pr_number().into(),
                    &User::default_user().name,
                )
                .await;
                Ok(tester)
            })
            .await;
    }

    #[sqlx::test]
    async fn approve_on_behalf(pool: sqlx::PgPool) {
        let world = World::default();
        world.default_repo().lock().set_config(
            r#"
[labels]
approve = ["+approved"]
"#,
        );
        BorsBuilder::new(pool)
            .world(world)
            .run_test(|mut tester| async {
                let approve_user = "user1";
                tester
                    .post_comment(format!(r#"@bors r={approve_user}"#).as_str())
                    .await?;
                assert_eq!(
                    tester.get_comment().await?,
                    format!(
                        "Commit pr-{}-sha has been approved by `{approve_user}`",
                        default_pr_number(),
                    ),
                );

                check_pr_approved_by(&tester, default_pr_number().into(), approve_user).await;
                Ok(tester)
            })
            .await;
    }

    #[sqlx::test]
    async fn insufficient_permission_approve(pool: sqlx::PgPool) {
        let world = World::default();
        world.default_repo().lock().permissions = Permissions::default();

        BorsBuilder::new(pool)
            .world(world)
            .run_test(|mut tester| async {
                tester.post_comment("@bors try").await?;
                assert_eq!(
                    tester.get_comment().await?,
                    "@default-user: :key: Insufficient privileges: not in try users"
                );
                Ok(tester)
            })
            .await;
    }

    #[sqlx::test]
    #[tracing_test::traced_test]
    async fn unapprove(pool: sqlx::PgPool) {
        let world = World::default();
        world.default_repo().lock().set_config(
            r#"
[labels]
approve = ["+approved"]
"#,
        );
        BorsBuilder::new(pool)
            .world(world)
            .run_test(|mut tester| async {
                tester.post_comment("@bors r+").await?;
                assert_eq!(
                    tester.get_comment().await?,
                    format!(
                        "Commit pr-{}-sha has been approved by `{}`",
                        default_pr_number(),
                        User::default_user().name
                    ),
                );
                check_pr_approved_by(
                    &tester,
                    default_pr_number().into(),
                    &User::default_user().name,
                )
                .await;
                tester.post_comment("@bors r-").await?;
                assert_eq!(
                    tester.get_comment().await?,
                    format!("Commit pr-{}-sha has been unapproved", default_pr_number()),
                );
                check_pr_unapproved(&tester, default_pr_number().into()).await;
                Ok(tester)
            })
            .await;
    }

    async fn check_pr_approved_by(
        tester: &BorsTester,
        pr_number: PullRequestNumber,
        approved_by: &str,
    ) {
        let pr_in_db = tester
            .db()
            .get_or_create_pull_request(&default_repo_name(), pr_number)
            .await
            .unwrap();
        assert_eq!(pr_in_db.approved_by, Some(approved_by.to_string()));
        let repo = tester.default_repo();
        let pr = repo.lock().get_pr(default_pr_number()).clone();
        pr.check_added_labels(&["approved"]);
    }

    async fn check_pr_unapproved(tester: &BorsTester, pr_number: PullRequestNumber) {
        let pr_in_db = tester
            .db()
            .get_or_create_pull_request(&default_repo_name(), pr_number)
            .await
            .unwrap();
        assert_eq!(pr_in_db.approved_by, None);
        let repo = tester.default_repo();
        let pr = repo.lock().get_pr(default_pr_number()).clone();
        pr.check_removed_labels(&["approved"]);
    }
}
