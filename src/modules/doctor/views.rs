pub const CSS: &str = include_str!("doctor.css");

pub enum LoginError {
  InvalidCredentials,
  RateLimited,
  ExpiredForm,
  Unavailable,
}

pub struct LoginView<'a> {
  pub csrf_token: &'a str,
  pub return_to: &'a str,
  pub error: Option<LoginError>,
}

pub fn login(view: LoginView<'_>) -> String {
  let (error, described_by) = match view.error {
        None => ("", ""),
        Some(error) => (
            match error {
                LoginError::InvalidCredentials => {
                    "No pudimos iniciar sesión con esos datos. Revisa la contraseña y el código e inténtalo de nuevo."
                }
                LoginError::RateLimited => {
                    "Se alcanzó el límite de intentos. Espera unos minutos antes de volver a intentarlo."
                }
                LoginError::ExpiredForm => {
                    "Este formulario venció. <a href=\"/login\">Abre un formulario nuevo</a> para volver a intentarlo."
                }
                LoginError::Unavailable => {
                    "El acceso todavía no está listo. La configuración inicial debe completarse antes de iniciar sesión."
                }
            },
            " aria-describedby=\"login-error\"",
        ),
    };
  let error_block = if error.is_empty() {
    String::new()
  } else {
    format!(
            "<div class=\"form-error\" id=\"login-error\" role=\"alert\" tabindex=\"-1\" autofocus>{error}</div>"
        )
  };
  let csrf_token = escape(view.csrf_token);
  let return_to = escape(view.return_to);
  document(
    "Iniciar sesión",
    &format!(
      r#"<main id="contenido" class="login-shell">
<h1>Iniciar sesión</h1>
<p class="intro">Accede al panel de tu consultorio con tu contraseña y el código de tu aplicación de autenticación.</p>
{error_block}
<form class="login-form" method="post" action="/login" aria-label="Iniciar sesión"{described_by}>
<input type="hidden" name="csrf_token" value="{csrf_token}">
<input type="hidden" name="return_to" value="{return_to}">
<div class="field">
<label for="password">Contraseña</label>
<input id="password" name="password" type="password" autocomplete="current-password" required>
</div>
<div class="field">
<label for="code">Código de autenticación</label>
<p class="field-hint" id="code-help">Escribe los 6 dígitos que muestra tu aplicación de autenticación.</p>
<input id="code" name="code" type="text" inputmode="numeric" autocomplete="one-time-code" pattern="[0-9]{{6}}" minlength="6" maxlength="6" aria-describedby="code-help" required>
</div>
<button class="primary-button" type="submit">Iniciar sesión</button>
<p class="session-hint">Tu sesión durará 30 días. Si compartes este dispositivo, cierra la sesión al terminar.</p>
</form>
</main>"#
    ),
  )
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum NavigationPage {
  Dashboard,
  Appointments,
  Contacts,
  Settings,
}

pub(crate) fn navigation(csrf_token: &str, settings: bool) -> String {
  navigation_page(
    csrf_token,
    if settings {
      NavigationPage::Settings
    } else {
      NavigationPage::Dashboard
    },
  )
}

pub(crate) fn navigation_page(csrf_token: &str, page: NavigationPage) -> String {
  let csrf_token = escape(csrf_token);
  let current = |target| {
    if page == target {
      " aria-current=\"page\""
    } else {
      ""
    }
  };
  format!(
    r##"<a class="skip-link" href="#contenido">Saltar al contenido</a>
<header class="site-header">
<nav class="navigation" aria-label="Navegación principal">
<a class="dashboard-link" href="/dashboard"{}>Mi consultorio</a>
<a href="/appointments"{}>Citas</a>
<a href="/contacts"{}>Conversaciones</a>
<a href="/settings"{}>Configuración</a>
<form method="post" action="/logout">
<input type="hidden" name="csrf_token" value="{csrf_token}">
<button class="logout-button" type="submit">Cerrar sesión</button>
</form>
</nav>
</header>"##,
    current(NavigationPage::Dashboard),
    current(NavigationPage::Appointments),
    current(NavigationPage::Contacts),
    current(NavigationPage::Settings),
  )
}

pub(crate) fn document(title: &str, body: &str) -> String {
  let title = escape(title);
  // Body is authored markup; escape external values where they enter that markup.
  format!(
    r#"<!doctype html>
<html lang="es">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<link rel="stylesheet" href="/assets/doctor.css">
</head>
<body>
<!-- Direction contract: approved narrow issue #4 shell, code-led.
THESIS: A Spanish task surface, not a branded analytics dashboard.
OWN-WORLD: Light neutral surfaces, sober blue actions, system type, native forms.
STORY: Sign in; read attention items before appointments; settings stay secondary.
FIRST VIEWPORT: Compact single-column login; dashboard title above two plain sections;
small top navigation wraps on mobile. Primary action follows the two login fields.
FORM: Pinned conventional Operate brief; no concept roll or imagery.
INTERACTION: Native submit, clear keyboard focus and linked error recovery; no motion.
FINISH: Isolated render and responsive inspection; backend integration is separate.
-->
{body}
</body>
</html>"#
  )
}

pub(crate) fn escape(value: &str) -> String {
  value
    .replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
    .replace('"', "&quot;")
    .replace('\'', "&#39;")
}
