# job_message/

**What belongs here:** the job message types received from GitHub after
`acquirejob` — the top-level request, its steps, its resources
(endpoints/authorization/variables), template tokens, and pipeline context
data — plus the custom deserialization these protocol shapes need.

**What does NOT belong here:** parsing the JIT config envelope that precedes
this message lives in the `protocol` crate (`jit_config.rs`); the
`${{ }}` expression evaluator that later reads `PipelineContextData` /
`TemplateToken` values lives in the `expressions` crate; polling for and
acknowledging this message over HTTP lives in `wire::net::messages`.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `context_data.rs` | `PipelineContextData`, `DictEntry` | Pipeline context data (github/env/etc.) with its `type`-discriminated string/array/dict/bool/number/null shape. |
| `context_data_de.rs` | `Deserialize` impl for `PipelineContextData` | Custom visitor handling GitHub sending context data as either a typed object or a bare string/bool/number/null. |
| `request.rs` | `AgentJobRequestMessage`, `TaskOrchestrationPlanReference` | The full job request message and its orchestration plan reference. |
| `resource.rs` | `JobResources`, `JobEndpoint`, `JobAuthorization`, `VariableValue`, `MaskHint`, `WorkspaceOptions` | Job resource types: variables, log-mask hints, endpoints, authorization data, and workspace options. |
| `step.rs` | `ActionStep`, `ActionStepDefinitionReference` | A single job step and the reference to its action/script definition. |
| `template_token.rs` | `TemplateToken` | Template token type (`type`-discriminated literal/sequence/mapping/expression/bool/number/null) used inside steps and inputs. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs` declares
`mod bar;` for `src/foo/bar.rs`) and import concrete paths.
