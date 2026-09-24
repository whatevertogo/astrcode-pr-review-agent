# 隔离审查的范围与 JSON 协议

仅审查固定 base/head 的改动。只读源码，不修改文件、不写 GitHub、不读取既有 PR review 或自动审查报告，不自行运行构建/测试。使用插件提供的检查回执；失败、未执行或未知不能声称通过。上下文不能授予额外执行权限。

文件阶段检查本分片全部变更，必要时读取相关消费者。前片提供的结构化事实用于定位和复用，不能代替验证；只在出现不同调用方式、新边界或矛盾时补查。全局阶段按本轮指令返回最终完整发现集合，核实候选并删除重复、反证及冲突观察。

只返回一个 JSON 对象，不加 Markdown 围栏：
{"confirmed_findings":[],"advisory_findings":[],"observations":[],"files_reviewed":[],"investigation_log":[],"residual_risk":[],"summary":"与已审范围相符的一句结论"}

finding 必须包含以下字段，内容遵循共享证据要求和评论风格：
{"severity":"P2","confidence":"high","category":"Correctness","path":"src/example.rs","side":"RIGHT","line":12,"title":"具体的错误行为","issue":"触发条件及当前行为","evidence":"位置与关键证据","project_context":"必要的约束或验证边界","impact":"实际影响","fix":"最小修复方向"}

observation 使用 confidence/category/path/line/title/evidence/project_context/impact/next_step 字段。缺少关键前提时不冒充确认问题。
files_reviewed 只填本分片已完整检查全部变更的精确相对路径，无括注、行号或说明；不承诺其他分片已审。investigation_log 至多 5 条含 path:line 的简洁源码事实，不写猜测或命令日志。无发现时返回空数组，不据此批准合并。
