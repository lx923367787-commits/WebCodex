use serde_json::{json, Value};

fn document_schema(operation: &str) -> Value {
    let mut properties = json!({
        "project": {"type":"string","description":"Exact registered project id. The configured document provider must have the same project root."},
        "review": {"type":"integer","enum":[0,1],"description":"LLM review requirement, not a verdict. Reads default to 0; generating, editing, converting or restoring a layout deliverable forces 1, including explicit 0. The effective value and reason are returned. Resume Jobs without this field."},
        "path": {"type":"string","description":"Project-relative source document path; never an absolute path."},
        "job_id": {"type":"string","description":"Resume an existing document Job. Supply project and job_id only; never restart an uncertain mutation."},
        "session_id": {"type":"string","description":"Optional explicit workflow session."},
        "expected_sha256": {"type":"string","pattern":"^[a-f0-9]{64}$","description":"Source SHA returned by a previous read. Required for edits and restoring a selected backup."},
        "options": {"type":"object","description":"Bounded format-specific options; unknown fields are rejected by the worker. present_document accepts review_probe:true to recompute actual source and current renderer/font dependencies without rendering images; use the prior review_material.render_options unchanged for comparison. Probe Job continuation uses project+job_id only.","additionalProperties":true}
    });
    let object = properties.as_object_mut().unwrap();
    if operation == "present_document" {
        object.extend(json!({
            "view":{"type":"string","enum":["auto","text","pages"],"default":"auto"},
            "cursor":{"type":"string","description":"Opaque source-SHA-bound continuation returned by this tool."},
            "export_file":{"type":"boolean","default":false,"description":"Also return an authenticated MCP ResourceLink to the original. This does not claim it becomes a chat attachment."}
        }).as_object().unwrap().clone());
    } else {
        if operation != "document_restore" {
            object.insert("output_path".into(),json!({"type":"string","description":"Project-relative destination for copy or conversion."}));
        }
        if operation == "document_edit" {
            object.insert("save_mode".into(),json!({"type":"string","enum":["copy","in_place"],"description":"in_place creates a recoverable backup and requires current source SHA. Defaults to copy when output_path is supplied, otherwise in_place."}));
            object.insert("operations".into(),json!({
                "type":"array","minItems":1,"maxItems":100,
                "items":{"type":"object","properties":{"op":{"type":"string","enum":["set_text","replace_text","set_format","append_paragraph","set_cell","accept_revision","reject_revision","set_cells","recalculate","insert_rows","delete_rows","insert_columns","delete_columns","set_notes","set_geometry","replace_image","set_chart_data","delete_pages","select_pages","reorder_pages","rotate_pages","merge","fill_form","annotate"]},"target":{"type":"object","additionalProperties":true}},"required":["op"],"additionalProperties":true},
                "description":"Use locations returned by present_document. Word: target.id, set_text(text), replace_text(find,text,occurrence), set_format, append_paragraph, set_cell, accept_revision/reject_revision. Excel: set_cells(sheet,cells[{coordinate,type,value}]), set_format(sheet,range,...), recalculate, row/column insert/delete(sheet,start,count); types text/number/boolean/formula/empty/date. PPT: target{slide:0,shape_id,paragraph:0}; set_text, replace_text, set_format, set_cell(row,column), set_notes, set_geometry(left,top,width,height,rotation in points/degrees), replace_image(image_path relative to project), set_chart_data(categories,series[{name,values}]). PDF pages are 1-based: delete/select/reorder_pages(pages), rotate_pages(pages,angle), merge(paths), fill_form(fields), annotate(page,rect,text,kind), replace_text(page,old_text,new_text,optional object_index; must match one complete text object). Formatting uses font_name,font_size,bold,italic,font_color(RRGGBB),alignment; Excel also number_format,fill_color,wrap_text,column_width,row_height. Unsupported format-specific operations fail explicitly.",
                "examples":[
                    [{"op":"replace_text","target":{"id":"word/document.xml#p0"},"find":"旧标题","text":"新标题","occurrence":0}],
                    [{"op":"set_cells","sheet":"Sheet1","cells":[{"coordinate":"B2","type":"number","value":42}]}],
                    [{"op":"set_text","target":{"slide":0,"shape_id":2,"paragraph":0},"text":"本周进展"}],
                    [{"op":"replace_text","page":1,"old_text":"Old title","new_text":"New title"}]
                ]
            }));
        }
        if operation == "document_restore" {
            object.remove("options");
            object.insert("backup_id".into(),json!({"type":"string","description":"Opaque backup identity returned by a prior document replacement."}));
            object.insert("offset".into(),json!({"type":"integer","minimum":0,"description":"List backup metadata for path when backup_id is omitted; continue at this bounded offset."}));
        }
    }
    json!({"type":"object","additionalProperties":false,"properties":properties,"required":["project"]})
}

pub fn present_document_input_schema() -> Value {
    document_schema("present_document")
}
pub fn document_edit_input_schema() -> Value {
    document_schema("document_edit")
}
pub fn document_convert_input_schema() -> Value {
    document_schema("document_convert")
}
pub fn document_restore_input_schema() -> Value {
    document_schema("document_restore")
}
