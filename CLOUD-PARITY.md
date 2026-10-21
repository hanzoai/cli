# Cloud coding session acceptance

Target: the cloud-session workflows in the supplied Claude Code field guide.
An API route alone does not count as end-to-end support. Pending means not yet
verified in production, including capabilities already present in the service.

| Workflow | Evidence / remaining acceptance |
| --- | --- |
| Public installation and authenticated local coding | v8.5.753 installer and three production E2Es passed |
| Independent cloud coding run | Production run completed after terminal detached |
| Commit, authenticated push and PR | Production smoke PRs #6 and #7 in hanzoai/cli; closed after verification |
| Follow-up after completion | Production follow-up read preceding run's committed file |
| Branch checkout from cloud | Production teleport fetched exact committed file |
| Parallel isolated machines and ports | Pending concurrent production acceptance |
| In-flight instructions | In progress: public caller-scoped steering and CLI handoff tracking |
| Queued instructions and retraction | Pending; interrupt-and-follow-up is not a retractable queue |
| Conversation continuation locally | Pending; branch checkout alone does not restore model context |
| Local repository bundle, including non-GitHub repositories | Pending |
| Plan / build / setup modes | Existing CLI selectors; pending full production acceptance |
| Environment selection, scripts, variables and services | Existing service; pending usable CLI workflow and acceptance |
| Network policies and external credential injection | Pending isolation and denied-host acceptance |
| Repository instructions, skills, hooks and MCP | Pending parity audit and production acceptance |
| GitHub personal / org onboarding and useful auth errors | Connected account verified; pending missing-App and SSO cases |
| CI and PR review auto-fix | Pending trigger-to-fix production acceptance |
| Scheduled, HTTP and GitHub triggered runs | Pending coding-specific workflow and acceptance |
| Coordinated projects | Pending |
| Browser, phone and desktop start / follow / steer | Pending browser workflow acceptance |
| Slack start / follow / steer | Existing integration tests; pending production acceptance |
| Diff review, inline comments and draft PRs | Pending |
| Share, archive and delete sessions | Pending ownership and visibility acceptance |
| Idle recovery and durable conversation | Pending restart/reclaim acceptance |
| Self-hosted execution and private networks | Pending |
| Usage, concurrency and clear billing errors | 402 identity fix deployed; pending limits acceptance |

Do not describe full parity as released until the pending rows have concrete
evidence. Keep release checks in E2E.md and the opt-in production tests; avoid
creating PRs, spending indefinitely, or modifying user repositories in default tests.
