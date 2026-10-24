"""Diagram schema, DOT isolation, and real optional Graphviz artifact tests."""
from copy import deepcopy
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

from PIL import Image
from pypdf import PdfReader

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from drawing.common import DrawingError
from drawing.diagrams import build_dot, normalize_spec, render_diagram


FLOW = {"kind": "flowchart", "title": "费用报销审批流程", "direction": "TB",
        "nodes": [{"id": "start", "label": "提交报销申请", "shape": "start"},
                  {"id": "check", "label": "资料是否完整？", "shape": "decision"},
                  {"id": "fix", "label": "补充发票和费用说明"},
                  {"id": "review", "label": "部门负责人审核"},
                  {"id": "save", "label": "登记财务系统", "shape": "database"},
                  {"id": "end", "label": "完成支付", "shape": "end"}],
        "edges": [{"id": "submit", "from": "start", "to": "check"},
                  {"from": "check", "to": "fix", "label": "否"},
                  {"from": "fix", "to": "check", "label": "重新提交"},
                  {"from": "check", "to": "review", "label": "是"},
                  {"from": "review", "to": "save"},
                  {"from": "save", "to": "end"}]}
ARCHITECTURE = {"kind": "architecture", "title": "本地办公助手系统架构", "direction": "LR",
                "groups": [{"id": "client", "label": "交互层"}, {"id": "service", "label": "服务层"},
                           {"id": "local", "label": "本地文件与工具"}],
                "nodes": [{"id": "chat", "label": "ChatGPT 网页对话", "group": "client"},
                          {"id": "gateway", "label": "MCP 接口与权限验证", "group": "service"},
                          {"id": "jobs", "label": "任务执行与结果确认", "group": "service"},
                          {"id": "office", "label": "Word / Excel 文档工具", "group": "local"},
                          {"id": "draw", "label": "流程图与架构图生成", "group": "local"},
                          {"id": "files", "label": "已授权办公文件夹", "shape": "database", "group": "local"}],
                "edges": [{"from": "chat", "to": "gateway", "label": "读取与修改请求"},
                          {"from": "gateway", "to": "jobs"}, {"from": "jobs", "to": "office"},
                          {"from": "jobs", "to": "draw"}, {"from": "office", "to": "files"},
                          {"from": "draw", "to": "files", "label": "保存可编辑源与图像"}]}


class DiagramSchemaTests(unittest.TestCase):
    def assert_invalid(self, spec, code):
        with self.assertRaises(DrawingError) as raised:
            normalize_spec(spec)
        self.assertEqual(raised.exception.code, code)

    def test_normalized_edge_ids_are_stable_and_input_is_not_mutated(self):
        source = deepcopy(FLOW)
        source["edges"][-1]["id"] = "edge_002"
        before = deepcopy(source)
        normalized = normalize_spec(source)
        self.assertEqual(source, before)
        self.assertEqual(normalize_spec(normalized), normalized)
        self.assertEqual(len({edge["id"] for edge in normalized["edges"]}), len(source["edges"]))
        self.assertEqual([(edge["from"], edge["to"]) for edge in normalized["edges"]],
                         [(edge["from"], edge["to"]) for edge in source["edges"]])

    def test_rejects_duplicate_ids_and_unknown_references(self):
        duplicate = deepcopy(FLOW)
        duplicate["nodes"][1]["id"] = "start"
        self.assert_invalid(duplicate, "duplicate_id")
        missing = deepcopy(FLOW)
        missing["edges"][0]["to"] = "absent"
        self.assert_invalid(missing, "unknown_reference")
        group = deepcopy(ARCHITECTURE)
        group["nodes"][0]["group"] = "absent"
        self.assert_invalid(group, "unknown_reference")
        group = deepcopy(ARCHITECTURE)
        group["groups"].append({"id": "unused", "label": "空层"})
        self.assert_invalid(group, "empty_group")

    def test_rejects_html_dot_attributes_and_graph_bombs(self):
        for field, value in (("dot", "digraph{a->b}"), ("command", "cmd /c whoami"), ("style", "neon")):
            spec = deepcopy(FLOW); spec[field] = value
            self.assert_invalid(spec, "invalid_spec")
        spec = deepcopy(FLOW); spec["nodes"][0]["label"] = '<IMG SRC="C:/private.png"/>'
        self.assert_invalid(spec, "invalid_text")
        spec = deepcopy(FLOW); spec["nodes"][0]["URL"] = "https://example.invalid"
        self.assert_invalid(spec, "invalid_spec")
        spec = deepcopy(FLOW); spec["nodes"][0]["id"] = 'a"; evil -> secret'
        self.assert_invalid(spec, "invalid_id")
        spec = deepcopy(FLOW); spec["nodes"][0]["label"] = "字" * 241
        self.assert_invalid(spec, "invalid_text")
        spec = deepcopy(FLOW); spec["nodes"] *= 14
        self.assert_invalid(spec, "diagram_too_large")

    def test_quotes_and_graphviz_escapes_are_literal_and_labels_wrap(self):
        spec = {"kind": "flowchart", "nodes": [{"id": "only", "label": '季度 "复核" \\N \\G; a -> b'}]}
        dot = build_dot(spec, "Microsoft YaHei")
        self.assertIn(r'\"复核\"', dot)
        self.assertIn(r'\\N', dot)
        self.assertNotIn('label=<', dot)
        wrapped = deepcopy(spec); wrapped["nodes"][0]["label"] = "跨部门业务协同与办公文件处理的长标签展示"
        dot = build_dot(wrapped, "Microsoft YaHei")
        self.assertIn(r"\n", dot)
        self.assertIn('id="node_only"', dot)

    def test_missing_engine_and_timeout_do_not_publish_outputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            with self.assertRaises(DrawingError) as raised:
                render_diagram(FLOW, root / "out", {"project_root": root, "graphviz_dot": "dot"})
            self.assertEqual(raised.exception.code, "missing_graphviz")
            fake = root / ("dot.exe" if os.name == "nt" else "dot")
            fake.write_bytes(b"fixture")
            with patch("drawing.diagrams.subprocess.run", side_effect=subprocess.TimeoutExpired([str(fake)], 60)) as invoke:
                with self.assertRaises(DrawingError) as raised:
                    render_diagram(FLOW, root / "out", {"project_root": root, "graphviz_dot": fake})
            self.assertEqual(raised.exception.code, "renderer_timeout")
            self.assertFalse(invoke.call_args.kwargs["shell"])
            self.assertIsInstance(invoke.call_args.args[0], list)
            self.assertGreater(invoke.call_args.kwargs["timeout"], 59)
            self.assertLessEqual(invoke.call_args.kwargs["timeout"], 60)
            self.assertFalse((root / "out" / "diagram.svg").exists())


@unittest.skipUnless(os.environ.get("WEBCODEX_TEST_GRAPHVIZ_DOT"), "Set WEBCODEX_TEST_GRAPHVIZ_DOT for real Graphviz rendering")
class GraphvizRenderingTests(unittest.TestCase):
    def test_real_dot_like_label_remains_text_and_cannot_add_edges(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            spec = {"kind": "flowchart", "nodes": [{"id": "only", "label": '\"; intruder -> only; \\N'}]}
            result = render_diagram(spec, root / "literal", {
                "project_root": root, "graphviz_dot": Path(os.environ["WEBCODEX_TEST_GRAPHVIZ_DOT"]),
                "font_name": "Microsoft YaHei"})
            svg = ET.parse(result["outputs"]["svg"]).getroot()
            texts = "".join(node.text or "" for node in svg.iter() if node.tag.endswith("}text"))
            self.assertIn("intruder -> only", texts)
            self.assertIn(r"\N", texts)
            self.assertEqual(sum(node.attrib.get("class") == "edge" for node in svg.iter()), 0)

    def test_real_chinese_flowchart_and_architecture_artifacts(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            context = {"project_root": root, "graphviz_dot": Path(os.environ["WEBCODEX_TEST_GRAPHVIZ_DOT"]),
                       "font_name": os.environ.get("WEBCODEX_TEST_FONT_NAME", "Microsoft YaHei")}
            for spec in (FLOW, ARCHITECTURE):
                output = root / spec["kind"]
                result = render_diagram(spec, output, context)
                self.assertEqual(result["node_count"], len(spec["nodes"]))
                self.assertEqual(result["edge_count"], len(spec["edges"]))
                svg = ET.parse(result["outputs"]["svg"]).getroot()
                self.assertIn(spec["title"], "".join(svg.itertext()))
                with Image.open(result["outputs"]["png"]) as picture:
                    self.assertGreater(min(picture.size), 100)
                self.assertEqual(len(PdfReader(result["outputs"]["pdf"]).pages), 1)
                with self.assertRaises(DrawingError) as raised:
                    render_diagram(spec, output, context)
                self.assertEqual(raised.exception.code, "target_exists")


if __name__ == "__main__":
    unittest.main()
