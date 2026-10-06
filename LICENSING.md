# Licensing / 许可证适用范围

This repository combines upstream WebCodex code with local customizations.
It is not an exclusively MIT-licensed replacement for upstream WebCodex.

| Material | Applicable terms |
| --- | --- |
| Code and documentation inherited from `yyjeqhc/webcodex`, including upstream portions of modified files | [Apache License 2.0](LICENSE), with existing copyright and attribution notices retained |
| Original additions and modifications whose copyright is owned by this repository's customization contributors | [MIT License](LICENSE-MIT), as authorized by their copyright owners |
| Third-party dependencies, bundled tools, fonts and assets | Their respective licenses; these are not relicensed by this repository |

`LICENSE-MIT` grants permission only for the original contributions that its
copyright holders own. A file containing both upstream code and original
changes retains the upstream Apache obligations as well as the applicable MIT
notice for those changes. It is not offered under an `Apache-2.0 OR MIT` choice
that would let a recipient disregard Apache conditions for inherited material.

The existing `LICENSE` is preserved verbatim. Existing package metadata that
identifies the upstream Apache license has not been changed to `MIT`: this task
does not relicense upstream packages or publish a new package. Packaging a mixed
distribution requires carrying both applicable license texts, the
[attribution notice](NOTICE), and any applicable third-party notices, and
describing its actual included components accurately.

Use the Git diff and commit history to identify local changes. This licensing
statement does not assert that every untracked file was written by the local
maintainer or that every third-party asset has been cleared for redistribution.
Copied material and contributions from other authors retain their original
terms unless their copyright owners explicitly grant additional permission.

Apache-2.0 redistribution also requires identifying modified files and retaining
relevant existing notices. The customization attribution in `NOTICE` and this
document explains the project-level changes; release preparation must retain
any existing file notices and make the changed-file provenance available.

## 中文说明

保留原 `LICENSE` 中的 Apache-2.0 全文；新增 `LICENSE-MIT` 用于定制维护者拥有版权的
原创新增及修改，署名为 `WebCodex customization contributors`。修改过的文件仍包含
上游部分时，需要同时遵守上游 Apache 条件；第三方依赖、字体和工具仍按各自许可使用。

不得把整个仓库或已有上游 npm 包直接宣称为纯 MIT，也不把两份许可写成可任意选择的
`Apache-2.0 OR MIT`。MIT 文本不赋予对其他作者代码的改许可权限。发布源码或安装包时，
保留适用的许可证、版权及第三方声明，并标明修改来源。

References: [Apache-2.0 terms, section 4](https://www.apache.org/licenses/LICENSE-2.0)
and the [MIT license text](https://opensource.org/license/mit).
