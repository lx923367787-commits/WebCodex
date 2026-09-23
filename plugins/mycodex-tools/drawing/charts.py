"""Read-only XLSX/CSV charts rendered with Matplotlib's noninteractive Agg backend.

Column integers are zero-based within source.range; strings select a unique
header. numeric_strings='decimal' explicitly permits strict decimal strings in
numeric series/scatter x; its default is 'error'. No formulas are evaluated.
The caller configures MPLCONFIGDIR and owns final artifact publication.
"""
from __future__ import annotations

import csv
from dataclasses import dataclass
from datetime import date, datetime
from decimal import Decimal, InvalidOperation
import hashlib
from io import BytesIO, StringIO
import math
from pathlib import Path
import re
import textwrap
import warnings
import zipfile

from openpyxl import load_workbook
from openpyxl.utils.cell import get_column_letter, range_boundaries
from mycodex_tools.common import Context, ToolError

from .common import DrawingError, safe_path, sha256

MAX_BYTES = 64 * 1024 * 1024
MAX_ROWS = 2000
MAX_COLUMNS = 64
MAX_SERIES = 8
PALETTE = ("#2563EB", "#0F766E", "#D97706", "#7C3AED", "#EA580C", "#0891B2", "#BE123C", "#475569")
DECIMAL_TEXT = re.compile(r"[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?\Z")


def _fail(code, message):
    raise DrawingError(code, message)


def _integer(value, low, high, name):
    if type(value) is not int or not low <= value <= high:
        _fail("invalid_chart_spec", f"{name} must be an integer from {low} to {high}.")
    return value


def _label(value, name, maximum=160):
    if not isinstance(value, str) or len(value) > maximum or any(ord(c) < 32 and c != "\n" for c in value):
        _fail("invalid_chart_spec", f"{name} must be text of at most {maximum} characters.")
    return value


@dataclass(frozen=True)
class Cell:
    value: object
    reference: str
    formula: bool = False
    error: bool = False


def _range(value, header):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z]{1,3}[1-9][0-9]{0,6}:[A-Za-z]{1,3}[1-9][0-9]{0,6}", value):
        _fail("invalid_range", "source.range must be an explicit A1 rectangle, for example A1:D13.")
    c1, r1, c2, r2 = range_boundaries(value.upper())
    rows = r2 - r1 + 1 - int(header)
    if c2 < c1 or r2 < r1 or c2 > 16384 or r2 > 1048576 or not 1 <= rows <= MAX_ROWS or c2-c1+1 > MAX_COLUMNS:
        _fail("chart_range_too_large", "Select 1..2000 data rows and at most 64 columns, excluding the optional header row.")
    return c1, r1, c2, r2


def _xlsx_rows(data, source, bounds):
    try:
        Context.validate_file_bytes(data, ".xlsx")  # Actual expanded bytes and every member CRC.
        with zipfile.ZipFile(BytesIO(data)) as archive:
            entries = archive.infolist()
            if len({e.filename for e in entries}) != len(entries):
                _fail("invalid_source", "Duplicate XLSX package members are unsupported.")
        # Both readers operate on the same immutable bytes. keep_links=False
        # only omits link objects from these read-only objects; nothing is saved.
        formulas = load_workbook(BytesIO(data), read_only=True, data_only=False, keep_links=False)
        values = load_workbook(BytesIO(data), read_only=True, data_only=True, keep_links=False)
    except DrawingError:
        raise
    except ToolError as error:
        _fail("source_too_large" if error.code == "archive_too_large" else "invalid_source", error.message)
    except Exception as error:
        _fail("invalid_source", f"Cannot read this XLSX file ({type(error).__name__}).")
    try:
        sheet = source.get("sheet")
        if sheet is None:
            if len(formulas.sheetnames) != 1:
                _fail("sheet_required", "Specify source.sheet when the workbook has multiple worksheets.")
            sheet = formulas.sheetnames[0]
        if not isinstance(sheet, str) or sheet not in formulas.sheetnames:
            _fail("sheet_not_found", "The selected worksheet does not exist.")
        c1, r1, c2, r2 = bounds
        args = {"min_row": r1, "max_row": r2, "min_col": c1, "max_col": c2}
        rows = []
        for row_number, (formula_row, value_row) in enumerate(zip(formulas[sheet].iter_rows(**args), values[sheet].iter_rows(**args)), r1):
            rows.append([Cell(v.value, f"{get_column_letter(c1+i)}{row_number}",
                              getattr(f, "data_type", None) == "f", getattr(v, "data_type", None) == "e" or getattr(f, "data_type", None) == "e")
                         for i, (f, v) in enumerate(zip(formula_row, value_row))])
        return rows, sheet
    finally:
        formulas.close()
        values.close()


def _csv_rows(data, source, bounds):
    if source.get("sheet") is not None:
        _fail("invalid_chart_spec", "CSV sources do not have worksheets.")
    encoding = source.get("encoding", "utf-8-sig")
    if encoding not in ("utf-8-sig", "utf-8", "gb18030"):
        _fail("invalid_chart_spec", "CSV encoding is utf-8-sig, utf-8 or explicitly selected gb18030.")
    try:
        decoded = data.decode(encoding)
    except UnicodeError:
        _fail("invalid_csv_encoding", "CSV decoding failed; set its explicit encoding instead of guessing.")
    c1, r1, c2, r2 = bounds
    literal_escape = source.get("literal_text_escape", "none")
    if literal_escape not in ("none", "apostrophe"):
        _fail("invalid_chart_spec", "source.literal_text_escape is none or apostrophe for a generated chart snapshot.")
    rows = []
    try:
        reader = csv.reader(StringIO(decoded, newline=""), strict=True)
        for row_number, row in enumerate(reader, 1):
            if row_number > r2:
                break
            if row_number >= r1:
                if literal_escape == "apostrophe":
                    row = [value[1:] if value.startswith("'") else value for value in row]
                rows.append([Cell(row[c-1] if c <= len(row) else None, f"{get_column_letter(c)}{row_number}") for c in range(c1, c2+1)])
    except csv.Error:
        _fail("invalid_csv", "CSV records are malformed.")
    while len(rows) < r2-r1+1:
        row_number = r1 + len(rows)
        rows.append([Cell(None, f"{get_column_letter(c)}{row_number}") for c in range(c1, c2+1)])
    return rows, None


def _read_value(cell):
    if cell.error:
        _fail("spreadsheet_error", f"Cell {cell.reference} contains an error. Fix the formula/data and save calculated values before charting.")
    if cell.formula and cell.value is None:
        _fail("formula_cache_missing", f"Cell {cell.reference} has no saved formula result. Recalculate and save with document_edit first, or provide computed values; this renderer never evaluates formulas.")
    return cell.value


def _column(selector, headers, column_count, role):
    if type(selector) is int:
        return _integer(selector, 0, column_count-1, role)
    if isinstance(selector, str):
        matches = [i for i, header in enumerate(headers) if header == selector]
        if len(matches) != 1:
            _fail("ambiguous_column" if matches else "column_not_found", f"{role} must identify exactly one header; use a zero-based range column index for duplicate headers.")
        return matches[0]
    _fail("invalid_chart_spec", f"{role} must be a header label or zero-based range column index.")


def _missing(value):
    return value is None or isinstance(value, str) and not value.strip()


def _number(value, reference, numeric_strings, conversions):
    if isinstance(value, str):
        if numeric_strings != "decimal":
            _fail("numeric_text_requires_opt_in", f"Cell {reference} is text, not a number. Explicitly set numeric_strings='decimal' for strict decimal conversion.")
        if not DECIMAL_TEXT.fullmatch(value):
            _fail("non_numeric_data", f"Cell {reference} is not a plain decimal number; currency, thousands separators and percentages are not guessed.")
        try:
            value = Decimal(value)
        except InvalidOperation:
            _fail("non_numeric_data", f"Cell {reference} cannot be converted to a finite decimal.")
        conversions.append(reference)
    if isinstance(value, bool) or not isinstance(value, (int, float, Decimal)):
        _fail("non_numeric_data", f"Cell {reference} must contain a numeric value; booleans/dates are not numeric series.")
    try:
        plotted = float(value)
    except (OverflowError, ValueError):
        _fail("number_out_of_range", f"Cell {reference} cannot be represented by the chart renderer.")
    if not math.isfinite(plotted) or value != 0 and plotted == 0:
        _fail("number_out_of_range", f"Cell {reference} is nonfinite or outside the chart renderer's numeric range.")
    return value


def _display(value):
    if isinstance(value, (date, datetime)):
        return value.isoformat()
    return str(value)


def _read_data(spec, context):
    source = spec.get("source")
    if not isinstance(source, dict) or set(source) - {"path", "sheet", "range", "header", "encoding", "literal_text_escape"}:
        _fail("invalid_chart_spec", "source requires path/range and accepts only sheet, header and encoding options.")
    root = Path(context["project_root"])
    data_root = Path(context.get("data_root", root))
    safe_path(root, source.get("path"), suffixes={".xlsx", ".csv"})
    path = safe_path(data_root, source.get("path"), suffixes={".xlsx", ".csv"}, must_exist=True)
    if path.stat().st_size > MAX_BYTES:
        _fail("source_too_large", "Chart source exceeds 64 MiB.")
    with path.open("rb") as stream:
        data = stream.read(MAX_BYTES + 1)
    if len(data) > MAX_BYTES:
        _fail("source_too_large", "Chart source grew beyond 64 MiB.")
    source_sha = hashlib.sha256(data).hexdigest()
    header = source.get("header", True)
    if type(header) is not bool:
        _fail("invalid_chart_spec", "source.header must be Boolean.")
    bounds = _range(source.get("range"), header)
    rows, sheet = (_xlsx_rows if path.suffix.lower() == ".xlsx" else _csv_rows)(data, source, bounds)
    c1, r1, c2, _ = bounds
    if header:
        headers = [_display(_read_value(cell)) if _read_value(cell) is not None else "" for cell in rows[0]]
        rows = rows[1:]
    else:
        headers = [get_column_letter(c) for c in range(c1, c2+1)]
    selected_x = _column(spec.get("x", 0), headers, c2-c1+1, "x")
    series = spec.get("series")
    if not isinstance(series, list) or not 1 <= len(series) <= MAX_SERIES:
        _fail("invalid_chart_spec", "series must contain 1..8 column definitions.")
    selected_series = []
    for item in series:
        if not isinstance(item, dict) or set(item) - {"column", "label"} or "column" not in item:
            _fail("invalid_chart_spec", "Each series requires column and accepts an optional label.")
        index = _column(item["column"], headers, c2-c1+1, "series.column")
        label = _label(item.get("label", headers[index] or get_column_letter(c1+index)), "series.label", 80)
        selected_series.append((index, label))
    if len({i for i, label in selected_series}) != len(selected_series) or len({label for i, label in selected_series}) != len(selected_series):
        _fail("ambiguous_series", "Series columns and displayed labels must each be unique.")
    if spec["chart_type"] == "pie" and len(selected_series) != 1:
        _fail("invalid_chart_spec", "A pie chart requires exactly one numeric series.")
    missing = spec.get("missing", "error")
    numeric_strings = spec.get("numeric_strings", "error")
    if missing not in ("error", "skip") or numeric_strings not in ("error", "decimal"):
        _fail("invalid_chart_spec", "missing is error|skip; numeric_strings is error|decimal.")
    x_type = spec.get("x_type", "auto")
    if x_type not in ("auto", "category", "number", "date"):
        _fail("invalid_chart_spec", "x_type is auto, category, number or date.")
    records, skipped, conversions, formulas = [], [], [], []
    for row_number, cells in enumerate(rows, r1 + int(header)):
        chosen = [cells[selected_x]] + [cells[index] for index, label in selected_series]
        values = [_read_value(cell) for cell in chosen]
        if isinstance(values[0], bool):
            _fail("invalid_category", f"Cell {chosen[0].reference} is Boolean; provide an explicit text category or numeric x value.")
        if any(_missing(value) for value in values):
            # skip is only permission to omit missing observations, not to hide
            # bad values in other selected cells of the same observation.
            for cell, value in zip(chosen[1:], values[1:]):
                if not _missing(value):
                    _number(value, cell.reference, numeric_strings, [])
            if not _missing(values[0]) and (spec["chart_type"] == "scatter" or x_type == "number"):
                _number(values[0], chosen[0].reference, numeric_strings, [])
            if missing == "error":
                missing_cells = ", ".join(c.reference for c, value in zip(chosen, values) if _missing(value))
                _fail("missing_data", f"Missing chart data at {missing_cells}; use missing='skip' to explicitly omit the whole row.")
            skipped.append(row_number)
            continue
        x = values[0]
        if spec["chart_type"] == "scatter" or x_type == "number":
            x = _number(x, chosen[0].reference, numeric_strings, conversions)
        elif x_type == "date":
            if isinstance(x, str):
                try:
                    x = date.fromisoformat(x) if len(x) == 10 else datetime.fromisoformat(x)
                except ValueError:
                    _fail("invalid_date", f"Cell {chosen[0].reference} must be an ISO-8601 date/time for x_type='date'.")
            if not isinstance(x, (date, datetime)):
                _fail("invalid_date", f"Cell {chosen[0].reference} must be a date/time.")
        elif x_type == "category":
            x = _display(x)
        elif not isinstance(x, (str, int, float, Decimal, date, datetime)) or isinstance(x, float) and not math.isfinite(x):
            _fail("invalid_category", f"Cell {chosen[0].reference} is not a supported category/date/numeric x value.")
        if len(_display(x)) > 120:
            _fail("category_too_long", f"Cell {chosen[0].reference} exceeds the 120-character chart label limit.")
        numeric = [_number(value, cell.reference, numeric_strings, conversions) for cell, value in zip(chosen[1:], values[1:])]
        formulas.extend(c.reference for c in chosen if c.formula)
        records.append((row_number, x, numeric))
    if not records:
        _fail("empty_chart", "No rows remain after the explicit missing-data policy.")
    if spec["chart_type"] == "pie":
        if len(records) > 20:
            _fail("too_many_categories", "Pie charts support at most 20 categories; aggregate the input or choose a bar chart.")
        if any(record[2][0] < 0 for record in records):
            _fail("negative_pie_value", "Pie charts cannot represent negative values; choose a bar chart.")
        if sum(Decimal(str(record[2][0])) for record in records) <= 0:
            _fail("empty_pie", "Pie values must have a positive total.")
    columns = {"x": {"selector": spec.get("x", 0), "index": selected_x, "column": get_column_letter(c1+selected_x), "header": headers[selected_x]},
               "series": [{"selector": item["column"], "index": index, "column": get_column_letter(c1+index), "header": headers[index], "label": label}
                          for item, (index, label) in zip(series, selected_series)]}
    metadata = {"source_path": source["path"], "source_sha256": source_sha, "source_bytes": len(data),
                "sheet": sheet, "range": source["range"].upper(), "header": header, "selected_columns": columns,
                "input_data_rows": len(rows), "data_rows": len(records), "point_rows": [r for r, x, y in records],
                "skipped_rows": skipped, "missing": missing, "row_reference_kind": "worksheet_row" if sheet is not None else "csv_record",
                "numeric_strings": numeric_strings, "numeric_string_conversions": len(conversions),
                "formula_cache": {"count": len(formulas), "sample_cells": formulas[:20], "sample_truncated": len(formulas)>20,
                                  "status": "saved_values_only; formulas are never calculated"},
                "numeric_rendering": "float64 geometry; data.csv retains full input numeric representations",
                "source_modified": False}
    if path.suffix.lower() == ".csv":
        metadata["encoding"] = source.get("encoding", "utf-8-sig")
    return records, columns, metadata, path


def _style(spec):
    style = spec.get("style", {})
    if not isinstance(style, dict) or set(style) - {"width", "height", "dpi", "font_size", "show_values"}:
        _fail("invalid_chart_style", "style accepts width, height, dpi, font_size and show_values only.")
    values = {"width": 10, "height": 6, "dpi": 160, "font_size": 12, "show_values": False, **style}
    for name, low, high in (("width", 6, 16), ("height", 4, 12), ("font_size", 9, 20)):
        if type(values[name]) not in (int, float) or not math.isfinite(values[name]) or not low <= values[name] <= high:
            _fail("invalid_chart_style", f"style.{name} must be between {low} and {high}.")
    _integer(values["dpi"], 96, 240, "style.dpi")
    if type(values["show_values"]) is not bool:
        _fail("invalid_chart_style", "style.show_values must be Boolean.")
    return values


def _make_figure(spec, records, columns, context, style):
    import matplotlib
    matplotlib.use("Agg", force=True)
    from matplotlib.backends.backend_agg import FigureCanvasAgg
    from matplotlib.figure import Figure
    from matplotlib.font_manager import FontProperties
    from matplotlib.ft2font import FT2Font
    from matplotlib.ticker import MaxNLocator

    font_path = Path(context["font_path"])
    if not font_path.is_file():
        _fail("font_missing", "Configure an existing local Chinese TTF/TTC font.")
    font = FontProperties(fname=str(font_path), size=style["font_size"])
    title = _label(spec.get("title", ""), "title")
    x_label = _label(spec.get("x_label", columns["x"]["header"]), "x_label")
    y_label = _label(spec.get("y_label", ""), "y_label")
    unit = _label(spec.get("unit", ""), "unit", 60)
    if unit:
        y_label = f"{y_label}（{unit}）" if y_label else unit
    labels = [_display(x) for row, x, values in records]
    series_labels = [s["label"] for s in columns["series"]]
    characters = set(title + x_label + y_label + "".join(labels) + "".join(series_labels) + "0123456789.-%")
    try:
        covered = FT2Font(str(font_path)).get_charmap()
    except (OSError, RuntimeError):
        _fail("font_invalid", "The configured font cannot be loaded by Matplotlib.")
    missing = [c for c in characters if not c.isspace() and ord(c) not in covered]
    if missing:
        _fail("font_missing_glyph", f"The configured font lacks U+{ord(sorted(missing)[0]):04X}; choose a font covering the chart labels.")
    figure = Figure(figsize=(style["width"], style["height"]), dpi=style["dpi"], layout="constrained", facecolor="white")
    FigureCanvasAgg(figure)
    axes = figure.subplots()
    axes.set_facecolor("white")
    kind = spec["chart_type"]
    axes.set_title(textwrap.fill(title, 38), fontproperties=font, fontsize=style["font_size"]+5,
                   color="#172033", loc="left", pad=18 if kind == "pie" else 54)
    plotted = [[float(values[i]) for row, x, values in records] for i in range(len(series_labels))]
    x_scale = "category"
    if kind == "pie":
        total = sum(Decimal(str(values[0])) for row, x, values in records)
        fractions = [float(Decimal(str(values[0]))/total) for row, x, values in records]
        wedges, _, percentages = axes.pie(fractions, startangle=90, counterclock=False,
                                           colors=[PALETTE[i % len(PALETTE)] for i in range(len(records))],
                                           autopct=lambda p: f"{p:.1f}%" if style["show_values"] and p >= 3 else "", pctdistance=.77,
                                           wedgeprops={"width": .5, "edgecolor": "white", "linewidth": 1.6})
        for label in percentages:
            label.set_fontproperties(font)
            label.set_fontsize(style["font_size"]-1)
        pie_labels = [f"{label}  {plotted[0][i]:.6g}" if style["show_values"] else label for i, label in enumerate(labels)]
        legend = axes.legend(wedges, [textwrap.fill(label, 22) for label in pie_labels], loc="center left", bbox_to_anchor=(1, .5), prop=font, frameon=False)
        axes.text(0, 0, textwrap.fill(series_labels[0], 12) + ("\n" + unit if unit else ""),
                  ha="center", va="center", fontproperties=font, color="#475569")
        axes.set_aspect("equal")
    else:
        if kind == "scatter":
            x_values = [float(x) for row, x, values in records]
            x_scale = "numeric"
        elif kind == "line" and all(isinstance(x, (int, float, Decimal)) and not isinstance(x, bool) for row, x, values in records):
            x_values = [float(x) for row, x, values in records]
            x_scale = "numeric"
        elif kind == "line" and all(isinstance(x, (date, datetime)) for row, x, values in records):
            x_values = [x for row, x, values in records]
            x_scale = "date"
        else:
            x_values = list(range(len(records)))
        for i, label in enumerate(series_labels):
            if kind == "bar":
                width = .8 / len(series_labels)
                bars = axes.bar([x + (i-(len(series_labels)-1)/2)*width for x in x_values], plotted[i],
                                width=width, label=label, color=PALETTE[i])
                if style["show_values"]:
                    if len(records)*len(series_labels) > 80:
                        _fail("too_many_value_labels", "show_values supports at most 80 bars; omit labels for dense data.")
                    axes.bar_label(bars, labels=[format(value, ".6g") for value in plotted[i]], padding=3, fontproperties=font, fontsize=style["font_size"]-1)
            elif kind == "line":
                axes.plot(x_values, plotted[i], label=label, color=PALETTE[i], linewidth=2.2,
                          marker="o" if len(records) <= 30 else None, markersize=4)
            else:
                axes.scatter(x_values, plotted[i], label=label, color=PALETTE[i], s=38, alpha=.8, edgecolors="white", linewidths=.45)
            if style["show_values"] and kind in ("line", "scatter"):
                if len(records)*len(series_labels) > 80:
                    _fail("too_many_value_labels", "show_values supports at most 80 points; omit labels for dense data.")
                for x, value in zip(x_values, plotted[i]):
                    axes.annotate(f"{value:.6g}", (x, value), xytext=(0, 7), textcoords="offset points",
                                  ha="center", fontproperties=font, fontsize=style["font_size"]-1)
        axes.set_xlabel(x_label, fontproperties=font, labelpad=10)
        axes.set_ylabel(y_label, fontproperties=font, labelpad=10)
        axes.grid(axis="y", color="#DCE3EC", linewidth=.8)
        axes.set_axisbelow(True)
        axes.axhline(0, color="#94A3B8", linewidth=.8, zorder=0)
        for side in ("top", "right"):
            axes.spines[side].set_visible(False)
        for side in ("left", "bottom"):
            axes.spines[side].set_color("#CBD5E1")
        axes.tick_params(axis="both", which="both", length=0, pad=6)
        axes.yaxis.set_major_locator(MaxNLocator(nbins=7))
        if x_scale == "category":
            step = max(1, math.ceil(len(records)/12))
            positions = list(range(0, len(records), step))
            if positions[-1] != len(records)-1:
                positions.append(len(records)-1)
            axes.set_xticks(positions, [textwrap.fill(labels[i], 14) for i in positions])
        legend = axes.legend(loc="lower left", bbox_to_anchor=(0, 1.015), ncols=min(4, len(series_labels)), prop=font, frameon=False, borderaxespad=0)
        for label in axes.get_xticklabels() + axes.get_yticklabels():
            label.set_fontproperties(font)
        axes.xaxis.get_offset_text().set_fontproperties(font)
        axes.yaxis.get_offset_text().set_fontproperties(font)
    if legend is not None:
        for label in legend.get_texts():
            label.set_color("#334155")
    return figure, {"font_name": font.get_name(), "font_path": str(font_path), "x_scale": x_scale,
                    "svg_text_mode": "embedded glyph paths", "style": style}


def _csv_bytes(records, columns):
    stream = StringIO(newline="")
    writer = csv.writer(stream, lineterminator="\n")
    escaped = 0
    def safe_value(value):
        nonlocal escaped
        if isinstance(value, str) and value[:1] in ("=", "+", "-", "@", "'", "\t", "\r"):
            escaped += 1
            return "'" + value  # Keep literal categories inert when opened in Excel.
        return _display(value)
    writer.writerow(["source_row", safe_value(columns["x"]["header"] or columns["x"]["column"])] + [safe_value(s["label"]) for s in columns["series"]])
    for row, x, values in records:
        writer.writerow([row, safe_value(x), *[safe_value(v) for v in values]])
    return stream.getvalue().encode("utf-8-sig"), escaped


def render_chart(spec: dict, output_dir: Path, context: dict) -> dict:
    allowed = {"kind", "title", "chart_type", "source", "x", "x_type", "series", "x_label", "y_label", "unit", "style", "missing", "numeric_strings"}
    if not isinstance(spec, dict) or set(spec) - allowed or spec.get("kind") != "chart" or spec.get("chart_type") not in ("bar", "line", "pie", "scatter"):
        _fail("invalid_chart_spec", "Use kind='chart' and chart_type=bar|line|pie|scatter with supported fields only.")
    if not isinstance(context, dict) or "project_root" not in context or "font_path" not in context:
        _fail("invalid_context", "The caller must provide project_root and an explicit local font_path.")
    output_dir = Path(output_dir).resolve(strict=True)
    if not output_dir.is_dir():
        _fail("invalid_output", "The caller must create the output directory first.")
    outputs = {"png": output_dir/"chart.png", "svg": output_dir/"chart.svg", "pdf": output_dir/"chart.pdf", "data_csv": output_dir/"data.csv"}
    if any(path.exists() for path in outputs.values()):
        _fail("output_exists", "A chart output already exists; choose an empty output directory.")
    style = _style(spec)
    records, columns, metadata, source_path = _read_data(spec, context)
    import matplotlib
    buffers = {}
    figure = None
    try:
        with matplotlib.rc_context({"text.usetex": False, "text.parse_math": False, "axes.unicode_minus": False,
                                    "pdf.fonttype": 42, "ps.fonttype": 42, "svg.fonttype": "path"}):
            figure, rendering = _make_figure(spec, records, columns, context, style)
            with warnings.catch_warnings(record=True) as observed:
                warnings.simplefilter("always")
                for extension in ("png", "svg", "pdf"):
                    stream = BytesIO()
                    figure.savefig(stream, format=extension, dpi=style["dpi"], facecolor="white")
                    buffers[extension] = stream.getvalue()
                if any("Glyph" in str(w.message) and "missing" in str(w.message) or "constrained_layout not applied" in str(w.message) for w in observed):
                    _fail("chart_layout_failed", "Labels do not fit or a font glyph is missing; shorten labels or increase the canvas size.")
    finally:
        if figure is not None:
            figure.clear()
    buffers["data_csv"], escaped = _csv_bytes(records, columns)
    # This is only a stale-source check; the caller performs its own final
    # identity/manifest verification before publishing the artifacts.
    checked_path = safe_path(Path(context.get("data_root", context["project_root"])), spec["source"]["path"], suffixes={".xlsx", ".csv"}, must_exist=True)
    if checked_path != source_path or sha256(checked_path) != metadata["source_sha256"]:
        _fail("source_changed", "The chart source changed while rendering; read it again before retrying.")
    for name, data in buffers.items():
        if len(data) > 16 * 1024 * 1024:
            _fail("chart_output_too_large", "A chart artifact exceeds 16 MiB; reduce the number of points or canvas size.")
    for name, path in outputs.items():
        try:
            with path.open("xb") as stream:
                stream.write(buffers[name])
        except FileExistsError:
            _fail("output_exists", "A chart output appeared concurrently; no existing output was overwritten.")
    metadata.update(rendering)
    metadata.update(kind="chart", chart_type=spec["chart_type"], title=spec.get("title", ""), csv_literal_text_escapes=escaped,
                    output_sha256={name: hashlib.sha256(data).hexdigest() for name, data in buffers.items()})
    snapshot_spec = {"kind": "chart", "chart_type": spec["chart_type"], "title": spec.get("title", ""),
                     "source": {"path": "data.csv", "range": f"A1:{get_column_letter(len(columns['series'])+2)}{len(records)+1}",
                                "header": True, "encoding": "utf-8-sig", "literal_text_escape": "apostrophe"},
                     "x": 1, "x_type": {"numeric": "number", "date": "date", "category": "category"}[rendering["x_scale"]],
                     "series": [{"column": i+2, "label": series["label"]} for i, series in enumerate(columns["series"])],
                     "x_label": spec.get("x_label", columns["x"]["header"]), "y_label": spec.get("y_label", ""),
                     "unit": spec.get("unit", ""), "missing": "error", "numeric_strings": "decimal", "style": style}
    notices = []
    if metadata["skipped_rows"]:
        notices.append({"code": "rows_skipped", "count": len(metadata["skipped_rows"]),
                        "message": "Rows with missing values were explicitly skipped; see metadata.skipped_rows and data.csv for exact references."})
    if metadata["formula_cache"]["count"]:
        notices.append({"code": "saved_formula_values", "count": metadata["formula_cache"]["count"],
                        "message": "The chart uses saved formula results; formulas were not recalculated during this run."})
    return {"outputs": {name: str(path) for name, path in outputs.items()}, "metadata": metadata,
            "snapshot_spec": snapshot_spec, "warnings": notices}
