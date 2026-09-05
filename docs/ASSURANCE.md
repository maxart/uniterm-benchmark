# Optional assurance rubric

Configuration templates start every criterion at `unknown`. This repository includes no product findings, scores or review evidence. Operators may perform their own source review against their exact selected revisions. A status in a config file is an operator assertion, not an independent assessment by the harness.

| Category | Criterion | Definition |
| --- | --- | --- |
| Security | Memory-safety boundary | Language and foreign-code boundaries, unsafe operations and the protections around them. |
| Security | Local IPC access control | Access restrictions and identity validation for local control sockets. |
| Security | Untrusted protocol input bounds | Limits and validation applied before allocating or processing untrusted input. |
| Security | Crash-safe persistence | Behavior of durable state when writes or processes fail. |
| Security | Update integrity | Verification and installation safeguards for in-product updates. |
| Security | Extension trust boundary | Authority and isolation of third-party extensions and hooks. |
| Security | Remote exposure and authentication | Network-accessible control surfaces and their transport/authentication requirements. |
| Security | Child-process containment | Process-group ownership, shutdown bounds and cleanup of child workloads. |
| Privacy | Telemetry and analytics | Collection and transmission of usage/analytics data. |
| Privacy | Default outbound network activity | Background network requests enabled by default. |
| Privacy | Local data retention | What data persists, where it lives, and its protection/retention behavior. |
| Privacy | Clipboard and host-data access | How and when clipboard or other host data is accessed. |
| Privacy | Remote data path | Where remote session data travels and which parties can access it. |
| Privacy | Network controls | Operator controls over optional network activity. |

Use exactly these names and definitions for every contender. `pass=100`, `partial=50`, and `fail=0`; `unknown` and `not_applicable` are excluded. Weights must be positive and finite. Feature absence is not automatically a favorable rating. Prefer unknown to an unsupported conclusion, and re-review whenever the selected revision changes.

Keep supporting review notes locally. Sanitized exports retain standard criterion names, statuses and weights, but omit free-text rationale and evidence paths. Reports explicitly identify these as operator-supplied ratings with withheld rationale. No rating contributes to the performance index. This rubric is not a penetration test, certification, legal opinion, or privacy guarantee.
