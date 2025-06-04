
use base64::{engine::general_purpose, Engine as _};
use crate::bindings::{exports::wasix::mcp::{router::{self, CallToolResult, Content::{Image, Text}, GetPromptResult, Guest, ImageContent, McpResource, Prompt, PromptError, ReadResourceResult, ResourceError, ServerCapabilities, TextContent, Tool, ToolError, Value}, secrets_list::{self, SecretsDescription}}, wasi::logging::logging::{log, Level}};

mod bindings {
    use crate::MockTool;
    wit_bindgen::generate!({ 
        generate_all,
        world: "mcp-secrets",
        path: "../../../wit"
     });


    export!(MockTool);
}

const WHITE_PIXEL: &[u8] = &[
    0xFF, 0xD8,                         // SOI (Start Of Image)
    0xFF, 0xE0, 0x00, 0x10,             // APP0 marker
    0x4A, 0x46, 0x49, 0x46, 0x00,       // 'JFIF\0'
    0x01, 0x01, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00,
    0xFF, 0xDB, 0x00, 0x43, 0x00,       // DQT marker
    0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08, 0x07, 0x07, 0x07, 0x09, 0x09,
    0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B, 0x0B, 0x0C, 0x19, 0x12, 0x13,
    0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A, 0x1C, 0x1C, 0x20, 0x24,
    0x2E, 0x27, 0x20, 0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28, 0x37, 0x29, 0x2C,
    0x30, 0x31, 0x34, 0x34, 0x34, 0x1F, 0x27, 0x39, 0x3D, 0x38, 0x32, 0x3C,
    0x2E, 0x33, 0x34, 0x32,
    0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x01, 0x00, 0x01, 0x03, 0x01, 0x11,
    0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01,
    0xFF, 0xC4, 0x00, 0x1F, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02,
    0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B,
    0xFF, 0xDA, 0x00, 0x0C, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00,
    0x3F, 0x00, 0xD2, 0xCF, 0x20, 0xFF, 0xD9,
];

struct MockTool;

impl secrets_list::Guest for MockTool {
    #[allow(async_fn_in_trait)]
    fn list_secrets() -> Vec::<SecretsDescription> {
        vec![]
    }
}

impl Guest for MockTool {
    #[allow(async_fn_in_trait)]
    fn name() -> String {
        "mock_tool".to_string()
    }

    #[allow(async_fn_in_trait)]
    fn instructions() -> String {
        "mock_tool is a testing tool".to_string()
    }

    #[allow(async_fn_in_trait)]
    fn capabilities() -> ServerCapabilities {
        ServerCapabilities {
            prompts: None,
            resources: None,
            tools: Some(router::ToolsCapability {
                list_changed: Some(true),
            }),
        }
    }

    #[allow(async_fn_in_trait)]
    fn list_tools() -> Vec::<Tool> {
        vec![
            Tool {
                name: "text_output".to_string(),
                description: "returns the same string as was used for input".to_string(),
                input_schema: Value {
                    json: r#"
                    {
                        "type": "object",
                        "properties": {
                            "input": {
                                "type": "string"
                            }
                        },
                        "required": ["input"]
                    }
                    "#.to_string(),
                },
                output_schema: Some(Value {
                    json: r#"
                    {
                        "type": "object",
                        "properties": {
                            "output": {
                                "type": "string"
                            }
                        },
                        "required": ["output"]
                    }
                    "#.to_string(),
                }),
            },
            Tool { 
                name: "file_output".to_string(), 
                description: "test for file output".to_string(), 
                input_schema: Value{json: "".to_string()}, 
                output_schema: None 
            }
        ]
    }


    #[allow(async_fn_in_trait)]
fn call_tool(tool_name:String,arguments:Value,) -> Result<CallToolResult,ToolError> {
    log(Level::Info, "mock_tool", format!("Got call for tool: {}",tool_name).as_str());
        match tool_name.as_str() {
            "text_output" => {
                Ok(CallToolResult {
                        content: vec![Text(TextContent {
                            text: arguments.json,
                            annotations: None,
                        })],
                        is_error: Some(false),
                })},
            "file_output" => {
                let base64_str: String = general_purpose::STANDARD.encode(WHITE_PIXEL);
                let data_uri = format!("data:image/jpeg;base64,{}", base64_str);

                Ok(CallToolResult { 
                    content: vec![
                        Image(
                            ImageContent {
                            data: data_uri,
                            mime_type: "image/jpeg".to_string(),
                            annotations: None,}
                        ),
                    ],
                    is_error: Some(false),
                })            
            },
            tool => {
                Err(ToolError::NotFound(tool.to_string()))
            }
            
        } 
    }

    #[allow(async_fn_in_trait)]
fn list_resources() -> Vec::<McpResource> {
        vec![]
    }

    #[allow(async_fn_in_trait)]
fn read_resource(uri:String,) -> Result<ReadResourceResult,ResourceError> {
       Err(ResourceError::NotFound(uri))
    }

    #[allow(async_fn_in_trait)]
fn list_prompts() -> Vec::<Prompt> {
        vec![]
    }

    #[allow(async_fn_in_trait)]
fn get_prompt(prompt_name:String,) -> Result<GetPromptResult,PromptError> {
        Err(PromptError::NotFound(prompt_name))
    }
}