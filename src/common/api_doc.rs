use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;
use utoipauto::utoipauto;

use super::config::Config;

#[utoipauto]
#[derive(OpenApi)]
#[openapi()]
pub struct ApiDoc;

/// Create the development API documentation using OpenAPI and Swagger UI.
pub fn swagger_ui(cfg: &Config) -> SwaggerUi {
  SwaggerUi::new(cfg.swagger_endpoint.clone()).url(
    cfg.swagger_endpoint.clone() + "/api-doc/openapi.json",
    ApiDoc::openapi(),
  )
}
