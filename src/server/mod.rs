pub mod request;
use chrono::Utc;
use mio::net::TcpStream;
use regex::RegexSet;
pub use request::*;
use std::collections::HashMap;
use std::fs::{OpenOptions, ReadDir};
// use std::io::{Error, Read};
pub use std::string::String;
// use std::time::{Duration, Instant};
use std::{fs, io, io::Write, path::Path};

pub mod response;
pub use response::*;

pub mod router;
pub use router::*;
pub mod session;
pub use session::*;
use tera::{Context, Tera};
pub mod cgi;
pub mod rendering_page;

pub use cgi::*;
pub use rendering_page::*;

use crate::{remove_prefix, remove_suffix, Config, Redirection};

#[derive(Debug)]
pub enum ServerError<'a> {
    IOError(&'a std::io::Error),
    TeraError(&'a tera::Error),
    TomlError(&'a toml::de::Error),
    RegexError(&'a regex::Error),
}

// -------------------------------------------------------------------------------------
// SERVER
// -------------------------------------------------------------------------------------
#[derive(Debug, Clone, Deserialize)]
pub struct Server {
    pub ip_addr: String,
    pub hostname: String,
    pub ports: Vec<u16>,
    pub root_directory: String,
    pub error_path: String,
    pub default_file: String,
    pub upload_limit: u32,
    pub accepted_methods: Vec<String>,
    pub directory_listing: bool,
    pub redirections: Vec<Redirection>,
    pub exclusion: Vec<String>,
}

impl Server {
    pub fn new(
        ip_addr: String,
        hostname: String,
        ports: Vec<u16>,
        root_directory: String,
        error_path: String,
        default_file: String,
        upload_limit: u32,
        accepted_methods: Vec<String>,
        directory_listing: bool,
        redirections: Vec<Redirection>,
        exclusion: Vec<String>,
    ) -> Self {
        Self {
            ip_addr,
            hostname,
            ports,
            root_directory,
            error_path,
            default_file,
            upload_limit,
            accepted_methods,
            directory_listing,
            redirections,
            exclusion,
        }
    }

    pub fn access_log(
        &self,
        request: &Request,
        config: &Config,
        status_code: u16,
        cookie: &String,
    ) {
        // Log request
        let mut tera = Tera::default();
        let res = tera.add_raw_template("access_log", &config.http.access_log_format);
        if res.is_err() {
            Self::error_log(
                request,
                config,
                "access_log",
                file!(),
                line!(),
                ServerError::TeraError(&res.err().unwrap()),
            );
            return;
        }

        let mut context = Context::new();

        let id_session = if let Some(p1) = cookie.split(";").into_iter().next() {
            let parts = p1.split("=").collect::<Vec<&str>>();
            if parts.len() == 2 {
                parts[1]
            } else {
                ""
            }
        } else {
            ""
        };

        let addr = format!("{}:{}{}", &request.host, &request.port, &request.location);
        context.insert("remote_addr", &addr);
        context.insert("remote_user", id_session);
        context.insert(
            "time_local",
            &format!("{}", Utc::now().format("%d-%m-%Y %H:%M:%S")),
        );
        context.insert("method", &format!("{: <6}", &request.method));
        context.insert("status", &status_code);
        context.insert(
            "bytes_sent",
            &format!("{: >8}", (request.length as f64) / 1000.0),
        );

        if let Ok(str) = tera.render("access_log", &context) {
            match OpenOptions::new()
                .append(true)
                .open(&config.log_files.access_log)
            {
                Ok(mut log_file) => {
                    let log_result = log_file.write((str + "\n").as_bytes());
                    match log_result {
                        Err(e) => Self::error_log(
                            &request,
                            config,
                            "access_log",
                            file!(),
                            line!(),
                            ServerError::IOError(&e),
                        ),
                        Ok(_) => (),
                    }
                }
                Err(_) => (),
            }
        }
    }

    pub fn error_log(
        request: &Request,
        config: &Config,
        func_name: &str,
        filename: &str,
        line_number: u32,
        error: ServerError,
    ) {
        let str = format!(
            "[{}]: {} - {}:{} - Func: {} at {}:{} - Error: {:?}\n",
            Utc::now().format("%d-%m-%Y %H:%M:%S"),
            format!("{: <5}", &request.method),
            request.host,
            request.port,
            func_name,
            filename,
            line_number,
            error
        );

        match OpenOptions::new()
            .append(true)
            .open(&config.log_files.error_log)
        {
            Ok(mut log_file) => {
                let log_result = log_file.write((str + "\n").as_bytes());
                match log_result {
                    Err(e) => eprintln!("Writing error. Err: {}", e),
                    Ok(_) => (),
                }
            }
            Err(_) => (),
        }
    }

    pub fn handle_redirection(
        &self,
        request: &Request,
        stream: &mut TcpStream,
        config: &Config,
        cookie: &String,
    ) {
        let mut redirects = self.redirections.clone();
        redirects.retain(|r| r.source == request.location);

        if redirects.len() > 0 {
            match self
                .redirections
                .iter()
                .any(|r| r.target == request.location)
            {
                true => Self::send_error_response(
                    &self,
                    stream,
                    &request,
                    config,
                    508,
                    "Loop Detected",
                    &cookie,
                ),
                false => {
                    // Construire la réponse de redirection
                    let response = format!(
                        "HTTP/1.1 302 Found\r\n\
                            Location: {}\r\n\
                            Content-Length: 0\r\n\
                            Connection: close\r\n\r\n",
                        redirects[0].target.clone()
                    );

                    // Envoyer la réponse via le TcpStream
                    stream.write_all(response.as_bytes()).unwrap();
                    let _ = stream.flush();
                    return;
                }
            }
        }
    }

    pub fn handle_request(
        &self,
        mut stream: &mut TcpStream,
        mut request: Request,
        cookie: String,
        config: &Config,
    ) {
 

        // Vérification de la méthode
        if !self
            .accepted_methods
            .iter()
            .any(|m| m.to_uppercase() == request.method.to_uppercase())
        {
            Self::send_error_response(
                &self,
                &mut stream,
                &request,
                config,
                405,
                "Method Not Allowed",
                &cookie,
            );
            return;
        }

        // Size limit
        if request.length > config.http.size_limit * 1024 {
            Self::send_error_response(
                &self,
                &mut stream,
                &request.clone(),
                config,
                413,
                "Content Too Large",
                &cookie,
            );
            return;
        }

        self.handle_redirection(&request, stream, config, &cookie);

        let location_path;
        // Chemin réel du fichier
        let mut root = self.root_directory.clone();
        root = remove_suffix(root, "/");

        let location = "./".to_string() + &root + &request.location;

        let discover = fs::read_dir(&location);
        let entries: ReadDir;
        let all;
        let mut dir_path;
        if !request.location.contains(".")
            && !request.location.contains("?")
            && request.method == "GET"
        {
            if !Path::new(&location.trim_end_matches("/")).exists() {
                Self::send_error_response(
                    &self,
                    &mut stream,
                    &request.clone(),
                    config,
                    404,
                    "Not Found x",
                    &cookie,
                );
                return;
            }
            location_path = "/index.html".to_string();
            dir_path = "src/static_files".to_string();
        } else {
            location_path = Self::check_and_clean_path(&request.location);
            dir_path = self.root_directory.clone();
        }

        if location_path.contains("/image") || location_path.contains("/css") {
            dir_path = "src/static_files".to_string();
        }

        let path = format!(
            "./{}/{}",
            remove_suffix(dir_path, "/"),
            remove_prefix(location_path, "/")
        ); // Chemin relatif au dossier public

        if !discover.is_err() && request.method == "GET" {
            entries = discover.unwrap();
            all = entries
                .filter_map(|entry| {
                    let el = entry.unwrap().path();
                    let name = el
                        .to_str()
                        .unwrap()
                        .strip_prefix(&location)
                        .unwrap()
                        .to_string();
                    let re_init = RegexSet::new(&self.exclusion);
                    if re_init.is_err() {
                        Self::error_log(
                            &request,
                            config,
                            "handle_request",
                            file!(),
                            line!(),
                            ServerError::RegexError(&re_init.err().unwrap()),
                        );
                        return None;
                    }

                    let re = re_init.unwrap();

                    match (el.is_file() && !re.is_match(&name))
                        || (el.is_dir() && self.directory_listing && !re.is_match(&name))
                    {
                        true => {
                            let entry_name = remove_prefix(name.clone(), "/");

                            Some(DirectoryElement {
                                entry: entry_name.clone(),
                                entry_type: match el.is_dir() {
                                    true => "folder".to_string(),
                                    _ => {
                                        let filename_parts =
                                            entry_name.split(".").collect::<Vec<&str>>();
                                        match filename_parts.len() {
                                            2 => {
                                                let ext = format!("{}{}", ".", filename_parts[1]);
                                                let mut file_formats: HashMap<&str, &str> =
                                                    HashMap::new();
                                                file_formats.insert(".rb", "ruby");
                                                file_formats.insert(".jpg", "image");
                                                file_formats.insert(".jpeg", "image");
                                                file_formats.insert(".png", "image");
                                                file_formats.insert(".txt", "text");

                                                match file_formats.get(ext.as_str()) {
                                                    Some(filetype) => filetype.to_string(),
                                                    None => "file".to_string(),
                                                }
                                            }
                                            _ => "file".to_string(),
                                        }
                                    }
                                },
                                link: request.location.clone() + &name,
                                is_directory: el.is_dir(),
                            })
                        }
                        false => None,
                    }
                })
                .collect::<Vec<DirectoryElement>>();

            self.handle_listing_directory(&mut stream, all, cookie, request.clone(), config);
            return;
        }

        let fieldname = Request::extract_field(&request, "name");
        println!("arret possible");

        if request.clone().location.contains("?") {
            let _ = self.create_folder(stream, &request.clone(), &*cookie.clone(), config);
        } else if request.clone().method == "POST" && fieldname == String::from("file_to_delete") {
            let _ = self.delete_elem(stream, &request.clone(), &*cookie.clone(), config);
        } else if request.clone().method == "POST" {
            self.upload_file(stream, &mut request, config)
        } else if Path::new(&path).exists() {
            // Servir un fichier statique
            self.handle_static_file(request.clone(), config, &mut stream, &path, cookie);
        } else {
            // Ressource introuvable
            Self::send_error_response(
                &self,
                &mut stream,
                &request.clone(),
                config,
                404,
                "Not Found",
                &cookie,
            );
        }
    }

    fn create_folder(
        &self,
        stream: &mut TcpStream,
        request: &Request,
        cookie: &str,
        config: &Config,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 1. Construire le chemin du dossier
        let folder_path = format!(
            "./{}{}",
            self.root_directory,
            Self::extract_folder_name(&request.location)
        );

        // 2. Vérifier si le dossier existe déjà pour éviter des erreurs inutiles
        if Path::new(&folder_path).exists() {
            Self::send_error_response(
                self,
                stream,
                &request.clone(),
                config,
                409, // Code HTTP 409 Conflict
                "Le dossier existe déjà",
                &cookie.to_string(),
            );
            return Ok(());
        }

        // 3. Créer le dossier
        match fs::create_dir(&folder_path) {
            Ok(_) => {
                // 4. Rediriger l'utilisateur vers l'URL d'origine (sans les paramètres de requête)
                let location = request.location.split('?').next().unwrap_or_default();
                self.send_redirect_response(stream, location)?;
            }
            Err(e) => {
                // 5. Gérer les erreurs de création de dossier
                Self::error_log(
                    request,
                    config,
                    "create_folder",
                    file!(),
                    line!(),
                    ServerError::IOError(&e),
                );
                Self::send_error_response(
                    self,
                    stream,
                    &request.clone(),
                    config,
                    500, // Code HTTP 500 Internal Server Error
                    "Erreur interne du serveur",
                    &cookie.to_string(),
                );
            }
        }

        Ok(())
    }

    fn delete_elem(
        &self,
        stream: &mut TcpStream,
        request: &Request,
        cookie: &str,
        config: &Config,
    ) {
        // 1. Construire le chemin du dossier
        let folder_path = format!(
            "./{}{}/{}",
            self.root_directory,
            request.location,
            Request::extract_field(request, "value")
        );

        if folder_path.len() == 0 {
            let _ = self.send_redirect_response(stream, "/");
            return;
        }
        // 2. Vérifier si le dossier n'existe pas pour éviter des erreurs inutiles
        if !Path::new(&folder_path).exists() {
            Self::send_error_response(
                self,
                stream,
                &request.clone(),
                config,
                400, // Code HTTP 400 Not Found
                "Bad Request :l'element choisit n'existe pas",
                &cookie.to_string(),
            );

            return;
        }

        if Path::new(&folder_path).is_dir() {
            // Supprimer le dossier
            let _ = fs::remove_dir_all(&folder_path);
        } else {
            // Supprimer le fichier
            let _ = fs::remove_file(&folder_path);
        }

        // Rediriger l'utilisateur vers l'URL d'origine (sans les paramètres de requête)
        let _ = self.send_redirect_response(stream, &request.location);
    }

    fn handle_static_file(
        &self,
        request: Request,
        config: &Config,
        stream: &mut TcpStream,
        path: &str,
        cookie: String,
    ) {
        // Déterminer le type de contenu en fonction de l'extension du fichier
        let mut to_cgi = false;
        let content_type = match Path::new(path).extension().and_then(|ext| ext.to_str()) {
            Some("html") => "text/html",
            Some("css") => "text/css",
            Some("js") => "application/javascript",
            Some("png") => "image/png",
            Some("jpg") | Some("jpeg") => "image/jpeg",
            Some("gif") => "image/gif",
            Some("json") => "application/json",
            Some("rb") => {
                to_cgi = true;
                "text/plain"
            }
            _ => "text/plain", // Type par défaut
        };

        // Lire le fichier
        match fs::read(path) {
            Ok(mut content) => {
                if to_cgi {
                    content = CGI::execute_file(path.to_string()).into();
                }

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n{}\r\n",
                    content_type,
                    content.len(),
                    cookie
                );

                if let Err(e) = stream.write_all(response.as_bytes()) {
                    Self::error_log(
                        &request,
                        config,
                        "handle_static_file",
                        file!(),
                        line!(),
                        ServerError::IOError(&e),
                    );
                } else {
                    // Log request
                    self.access_log(&request, config, 200, &cookie);
                    let _ = stream.flush();
                }
                if let Err(e) = stream.write_all(&content) {
                    Self::error_log(
                        &request,
                        config,
                        "handle_static_file",
                        file!(),
                        line!(),
                        ServerError::IOError(&e),
                    );
                }
            }
            Err(e) => {
                Self::error_log(
                    &request,
                    config,
                    "handle_static_file",
                    file!(),
                    line!(),
                    ServerError::IOError(&e),
                );
                Self::send_error_response(
                    &self,
                    stream,
                    &request,
                    config,
                    500,
                    "Internal Server Error",
                    &cookie,
                );
            }
        }
    }

    /// Gère une requête pour un fichier statique.
    fn handle_listing_directory(
        &self,
        stream: &mut TcpStream,
        all: Vec<DirectoryElement>,
        cookie: String,
        request: Request,
        config: &Config,
    ) {
        // Chargement du template
        let tera = Tera::new("src/**/*.html").unwrap();
        let mut context = Context::new();
        context.insert("elements", &all);
        context.insert("size", &all.len());
        context.insert("hostname", &self.hostname);

        match tera.render(&self.default_file.strip_prefix("src/").unwrap(), &context) {
            Ok(content) => {
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n{}\r\n{}",
                    content.len(),
                    cookie,
                    content
                );

                if let Err(e) = stream.write_all(response.as_bytes()) {
                    Self::error_log(
                        &request,
                        config,
                        "handle_listing_directory",
                        file!(),
                        line!(),
                        ServerError::IOError(&e),
                    );
                } else {
                    // Log request
                    self.access_log(&request, config, 200, &cookie);
                    let _ = stream.flush();
                }
            }
            Err(e) => {
                Self::error_log(
                    &request,
                    config,
                    "handle_listing_directory",
                    file!(),
                    line!(),
                    ServerError::TeraError(&e),
                );
                Self::send_error_response(
                    &self,
                    stream,
                    &request,
                    config,
                    500,
                    "Internal Server Error",
                    &cookie,
                );
            }
        }
    }

    /// Envoie une réponse d'erreur HTTP.
    fn send_error_response(
        &self,
        stream: &mut TcpStream,
        request: &Request,
        config: &Config,
        status_code: u16,
        status_message: &str,
        cookie: &String,
    ) {
        // Chargement du template
        let tera = Tera::new("src/**/*.html").unwrap();
        let mut context = Context::new();
        context.insert(
            "error",
            &(HTMLError {
                code: status_code,
                status: status_message.to_string(),
            }),
        );

        match tera.render(&self.error_path.strip_prefix("src/").unwrap(), &context) {
            Ok(content) => {
                let response = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
                    status_code,
                    status_message,
                    content.len(),
                    content
                );
                if let Err(e) = stream.write_all(response.as_bytes()) {
                    Self::error_log(
                        &request,
                        config,
                        "send_error_response",
                        file!(),
                        line!(),
                        ServerError::IOError(&e),
                    );
                } else {
                    self.access_log(&request, config, status_code, &cookie);
                    let _ = stream.flush();
                }
            }
            Err(e) => {
                Self::error_log(
                    &request,
                    config,
                    "send_error_response",
                    file!(),
                    line!(),
                    ServerError::TeraError(&e),
                );
            }
        }
    }

    fn upload_file(&self, stream: &mut TcpStream, request: &mut Request, config: &Config) {
        // Vérifier si le nom du fichier est vide
        if !request.complete {
            self.send_error_response(
                stream,
                &request.clone(),
                config,
                400,
                "Bad Request: No file uploaded",
                &request.id_session,
            );
            return;
        }

        // Vérifier si la taille du fichier est nulle
        if request.body.len() == 0 {
            self.send_error_response(
                stream,
                &request.clone(),
                config,
                400,
                "Bad Request: File size is zero",
                &request.id_session,
            );
            return;
        }

        // Obtention du nom du fichier
        let filename = Request::extract_field(request, "filename");

        // Créer le chemin du fichier
        let filepath = format!("./{}{}/{}", self.root_directory, request.location, filename);

        // Ouvrir ou créer le fichier
        let mut file = match OpenOptions::new().create(true).write(true).open(&filepath) {
            Ok(file) => file, // Déballer le fichier
            Err(err) => {
                Self::error_log(
                    request,
                    config,
                    "upload_file",
                    file!(),
                    line!(),
                    ServerError::IOError(&err),
                );
                self.send_error_response(
                    stream,
                    &request.clone(),
                    config,
                    500,
                    &format!("Internal Server Error: Failed to open file - {}", err),
                    &request.id_session,
                );
                return;
            }
        };

      //  let file_content = Request::extract_field(request, "value");
        println!("content-type{}", request.content_type);
        let file_content = Request::extract_values(&request.body_byte,request.boundary.clone().unwrap_or_default());
        // Écrire le contenu du fichier
        if let Err(err) = file.write_all(&file_content) {
            Self::error_log(
                request,
                config,
                "upload_file",
                file!(),
                line!(),
                ServerError::IOError(&err),
            );
            self.send_error_response(
                stream,
                &request.clone(),
                config,
                500,
                "Internal Server Error: Failed to write to file",
                &request.id_session,
            );
            return;
        }

        // Envoyer une réponse de redirection
        match self.send_redirect_response(stream, &*request.location) {
            Ok(_) => {
                self.access_log(&request.clone(), config, 200, &request.id_session);
                return;
            }
            Err(e) => {
                Self::error_log(
                    request,
                    config,
                    "upload_file",
                    file!(),
                    line!(),
                    ServerError::IOError(&e),
                );
                self.send_error_response(
                    stream,
                    &request.clone(),
                    config,
                    500,
                    &format!("Internal Server Error: Failed to send redirect - {}", e),
                    &request.id_session,
                );
                return;
            }
        }
    }

    fn extract_folder_name(loaction: &str) -> String {
        let location = loaction.split('?').nth(0).unwrap();
        if let Some(folder_name_part) = loaction
            .split('?')
            .nth(1)
            .unwrap_or_default()
            .split("foldername=")
            .nth(1)
        {
            let folder_name = folder_name_part
                .trim_matches(&['"', '+'])
                .trim()
                .to_string();
            return format!("{}/{}", location, folder_name);
        }
        String::new()
    }

    pub fn send_redirect_response(&self, stream: &mut TcpStream, location: &str) -> io::Result<()> {
        // Construire la réponse HTTP
        let response = format!(
            "HTTP/1.1 302 Found\r\n\
             Location: {}\r\n\
             Content-Length: 0\r\n\
             Cache-Control: no-cache, no-store, must-revalidate\r\n\
             Pragma: no-cache\r\n\
             Expires: 0\r\n\
             \r\n",
            location
        );
        match stream.write_all(response.as_bytes()) {
            Ok(_) => println!("Response sent successfully."),
            Err(e) => println!("Failed to send response: {}", e),
        }
        match stream.flush() {
            Ok(_) => println!("Stream flushed successfully."),
            Err(e) => println!("Failed to flush stream: {}", e),
        }
        Ok(())
    }

    fn check_and_clean_path(path: &str) -> String {
        // Trouver l'index du motif "images/" ou "css/"
        if let Some(index) = path.find("/images/").or_else(|| path.find("/css/")) {
            // Supprimer tout ce qui se trouve avant le motif
            let cleaned_path = &path[index..];
            cleaned_path.to_string()
        } else {
            // Retourner le chemin original si aucun motif n'est trouvé
            path.strip_prefix("/").unwrap().to_string()
        }
    }
}
// -------------------------------------------------------------------------------------
