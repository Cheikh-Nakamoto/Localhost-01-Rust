use chrono::Utc;
use mio::net::TcpStream;
use regex::Regex;
use std::{collections::HashMap, io::Read};

use crate::{get_boundary, get_content_length, remove_prefix, remove_suffix};

// -------------------------------------------------------------------------------------
// REQUEST
// -------------------------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct Request {
    pub id_session: String,
    pub content_type: String,
    pub content_length: Option<usize>,
    pub location: String,
    pub host: String,
    pub port: u16,
    pub method: String,
    pub head: String,
    pub body: String,
    pub body_byte: Vec<u8>,
    pub filename: String,
    pub length: usize,
    pub reference: String,
    pub boundary: Option<String>,
    pub complete: bool,
    pub headers: HashMap<String, String>,
    pub timestamp: i64,
}

impl Request {
    pub fn new(
        id_session: String,
        content_type: String,
        location: String,
        host: String,
        port: u16,
        method: String,
        head: String,
        body: String,
        body_byte: Vec<u8>,
        filename: String,
        length: usize,
        reference: String,
    ) -> Self {
        Self {
            id_session,
            content_type,
            content_length: None,
            location,
            host,
            port,
            method,
            head,
            body,
            body_byte,
            filename,
            length,
            reference,
            boundary: None,
            complete: false,
            headers: HashMap::new(),
            timestamp: Utc::now().timestamp_millis(),
        }
    }

    pub fn default() -> Self {
        Request::new(
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            0,
            String::new(),
            String::new(),
            String::new(),
            vec![],
            String::new(),
            0,
            String::new(),
        )
    }

    pub fn stream_to_str(stream: &mut TcpStream) -> (String, Vec<u8>) {
        let mut buffer = [0; 8192]; // Buffer de 8 Ko
        let mut request_str = String::new();
        let mut buff_complete = vec![];

        // ---------------------------------------------
        // Autre manière de lire à tester après
        // ---------------------------------------------
        // let mut reader = BufReader::new(stream);
        // let mut data = Vec::new();
        // reader.read_to_end(&mut data);
        // request_str.push_str(&String::from_utf8_lossy(&data));
        // ---------------------------------------------

        loop {
            match stream.read(&mut buffer) {
                Ok(n) => {
                    let buff = String::from_utf8_lossy(&buffer[..n]);
                    request_str.push_str(&buff);
                    buff_complete.extend_from_slice(&buffer[..n]);

                    // if let Some(pos) = request_str.find(&new_line_pattern) {
                    //     headers_end = Some(pos);
                    // }
                }
                Err(_) => {
                    break;
                }
            }
        }
        (request_str, buff_complete)
    }

    pub fn read_request(stream: &mut TcpStream) -> Self {
        let new_line_pattern = "\r\n\r\n";
        let mut request = Request::default();
        let (request_str, body_byte) = Self::stream_to_str(stream);
        let mut is_post = false;

        if request_str.starts_with("GET") {
            request.complete = true;
            request.method = String::from("GET");
            request.head = request_str.clone();
        } else if request_str.starts_with("POST") {
            is_post = true;
            request.method = String::from("POST");
        } else {
            request.body = request_str.clone();
            request.body_byte = body_byte.clone();
            return request;
        }

        // Vérification de la présence des 2 parties de la requête
        match request_str.find(new_line_pattern) {
            None => {
                return request;
            }
            Some(header_limit) => {
                let headers = &request_str[..header_limit];

                Request::parse_http_request(headers, &mut request);

                let mut form_data: Vec<HashMap<&str, Option<String>>> = vec![]; // Chaque HashMap représente un champ du formulaire.

                if is_post {
                    let mut head = request_str.clone();
                    let mut body = head.split_off(header_limit);
                    body = body.strip_prefix(new_line_pattern).unwrap().to_string();

                    request.head = head.clone();
                    request.body = body.clone();
                    request.body_byte = body_byte.clone();

                    if let Some(content_length_str) = get_content_length(&head) {
                        match content_length_str.parse::<usize>() {
                            Ok(val) => {
                                request.content_length = Some(val);
                                if body.len() >= val {
                                    request.complete = true;
                                }
                            }
                            Err(_) => (),
                        }
                    }

                    request.boundary = get_boundary(&request_str);
                    let boundary = request.boundary.clone().unwrap_or_default();

                    Self::extract_form_data(&body, boundary, &mut form_data);

                    // A partir d'ici tu peux placer la fonction qui permet d'utiliser les données collectées
                    // Par exemple enregistrer l'image, pour le cas de la création de dossier tu auras ici
                    // Le nom du dossier à créer
                    // Tu sauras comment mettre à jour la variable request avec ces données collectées.

                    if let Some(hashmap) = form_data.get(0) {
                        if let Some(Some(file)) = hashmap.get("filename") {
                            request.filename = file.to_string();
                        }
                        if let Some(Some(file)) = hashmap.get("content_type") {
                            request.content_type = file.to_string();
                        }
                    }
                }
                request
            }
        }

        // ----------------------------------------------------------------------------------------
        // Ancien bloc `if is_post {...}`
        // ----------------------------------------------------------------------------------------
        //     if is_post {
        //         let body_start = headers_end + 4;
        //         let body_already_not_read = request.length - body_start;

        //         let mut body = vec![0; request.length];

        //         if body_already_not_read > 0 {
        //             body = request_str.as_bytes()[body_start..].to_vec();
        //             if let Some(pos) = body.windows(4).position(|elem| elem == b"\r\n\r\n") {
        //                 // println!("trouvé dexieme delimiteur a l'index : {}", pos);
        //                 let sec_header = String::from_utf8_lossy(&body[..pos]).into_owned();
        //                 body = body[pos + 4..].to_vec();
        //                 let liste = sec_header.lines().collect::<Vec<&str>>();
        //                 let filename = Self::extract_header_value(&liste, "Content-Disposition");
        //                 println!("filename1 {} \n\n", filename);
        //                 request.filename = filename.replace('"', "").to_string();
        //             }
        //             if request.filename.is_empty() {
        //                 let liste = headers.lines().collect::<Vec<&str>>();
        //                 let filename = Self::extract_header_value(&liste, "Content-Disposition");
        //                 println!("filename2 {} \n\n", filename);
        //                 request.filename = filename.replace('"', "").to_string();
        //             }
        //         }

        //         let tmp = String::from_utf8_lossy(&body);
        //         // let liste = &tmp.to_owned().lines().collect::<Vec<&str>>();
        //         // let content_disposition = Self::extract_header_value(liste, "Content-Disposition");
        //         // println!("value {}", content_disposition);
        //         //    println!("body {} \n\n",tmp);

        //         request.body = tmp.to_string();
        //     }
        // ----------------------------------------------------------------------------------------
    }

    pub fn extract_form_data(
        body: &String,
        boundary: String,
        form_data: &mut Vec<HashMap<&str, Option<String>>>,
    ) {
        let new_line_pattern = "\r\n\r\n";
        let body_parts = body
            .split(boundary.as_str())
            .map(|s| {
                remove_suffix(remove_prefix(s.to_string(), "\r\n"), "\r\n--")
                    .replace(new_line_pattern, "; value=")
            })
            .collect::<Vec<String>>();

        // Tu peux jeter un coup d'œil sur la docu pour comprendre la syntaxe
        // https://docs.rs/regex/latest/regex/index.html
        let re = Regex::new(
            r#"(?xs)
                    (?:Content-Disposition:\s*
                    (?<content_disposition>[^;]+);\s*)?
                    (?:name="(?<name>[^"]+)"\s*)?
                    (?:\s*;\s*
                        (?:filename="(?<filename>[^"]+)"\s*)?
                        (?:file_to_delete="(?<file_to_delete>[^"]+)"\s*)?
                        (?:Content-Type:\s*(?<content_type>[^;]+)\s*)?
                    )*
                    ;\s*value=(?<value>.*)?
                    "#,
        )
        .unwrap();

        // Ici on parcourt les différentes parties du body pour voir si les champs recherchés sont là
        body_parts.iter().for_each(|s| {
            if let Some(caps) = re.captures(&s) {
                let mut values = HashMap::new();
                values.insert(
                    "content_disposition",
                    Some(caps["content_disposition"].to_string()),
                );

                values.insert("name", Some(caps["name"].to_string()));
                values.insert(
                    "filename",
                    caps.name("filename")
                        .map_or(None, |m| Some(m.as_str().to_string())),
                );
                values.insert(
                    "content_type",
                    caps.name("content_type")
                        .map_or(None, |m| Some(m.as_str().to_string())),
                );
                values.insert(
                    "file_to_delete",
                    caps.name("file_to_delete")
                        .map_or(None, |m| Some(m.as_str().to_string())),
                );
                values.insert("value", Some(caps["value"].to_string()));
                form_data.push(values);
            }
        });
    }

    pub fn parse_http_request(request_str: &str, request: &mut Request) {
        let mut location = String::new();
        let mut host = String::new();
        let mut port: u16 = 0;
        let mut cookie = String::new();
        let mut headers = HashMap::new();

        let lines: Vec<&str> = request_str.lines().collect();

        // Parser la première ligne (ex: "GET /index.html HTTP/1.1")
        if !lines.is_empty() {
            let parts: Vec<&str> = lines[0].split_whitespace().collect();
            if parts.len() >= 2 {
                location = parts[1].to_string(); // URL (/index.html)
            }
        }

        // Parser les en-têtes
        for line in lines.iter().skip(1) {
            if line.starts_with("Host:") {
                let host_parts: Vec<&str> = line.split(":").collect();
                host = host_parts[1].trim().to_string();
                if host_parts.len() > 2 {
                    port = host_parts[2].parse::<u16>().unwrap_or(80);
                }
            } else if line.contains(":") {
                let mut parts = line.splitn(2, ":");
                if let (Some(key), Some(value)) = (parts.next(), parts.next()) {
                    let key = key.trim().trim_matches('"').to_string(); // Supprimer les espaces et les guillemets
                    if key == "Cookie" {
                        cookie = value.to_owned();
                    }
                    let value = value.trim().to_string(); // Supprimer les espaces
                    if !key.is_empty() && !value.is_empty() {
                        headers.insert(key, value);
                    }
                }
            }
        }

        let binding = Self::extract_header_value(&lines, "Referer:");
        let referer = binding.split(":").nth(1).unwrap_or_default();

        request.location = location;
        request.id_session = cookie
            .trim()
            .strip_prefix("cookie_01=")
            .unwrap_or_default()
            .to_owned();
        request.host = host;
        request.port = port;
        request.length = request.body.len();
        request.reference = referer.to_string();
    }

    pub fn extract_header_value(headers: &[&str], pattern: &str) -> String {
        let mut header_value = String::new();

        for line in headers {
            if line.starts_with(pattern) {
                let cookie_str = line.trim_start_matches(pattern).trim();
                for cookie in cookie_str.split(';') {
                    let mut parts = cookie.trim().splitn(2, '=');
                    if let (Some(_), Some(value)) = (parts.next(), parts.next()) {
                        header_value = value.to_string();
                    }
                }
            }
        }
        header_value
    }
    pub fn extract_field(request: &Request, fieldname: &str) -> String {
        let mut filename = String::new();
        let mut form_data = vec![];
        if let Some(boundary) = &request.boundary {
            Request::extract_form_data(&request.body, boundary.to_string(), &mut form_data);
        }
        for field in form_data {
            if let Some(Some(name)) = field.get(fieldname) {
                filename = name.to_owned();
            }
        }
        filename
    }
    /*
       La fonction cherche sucessivement le paterne \r\n\r\n puis le bopundary
       et encore \r\n\r\n et separe a chaque fois !
    */
    pub fn extract_values(body: &[u8], boundary: String) -> Vec<u8> {
        let new_line_pattern = b"\r\n\r\n";
        let start_boundary_pattern = format!("\r\n--{}", boundary).into_bytes();
        let start_pos = body
            .windows(new_line_pattern.len())
            .position(|window| window == new_line_pattern)
            .unwrap_or_default();

        let headers_end = start_pos + new_line_pattern.len();
        let start_pos_body = body[headers_end..]
            .windows(start_boundary_pattern.len())
            .position(|window| window == start_boundary_pattern)
            .unwrap_or_default();

        let file_end = headers_end + start_pos_body;
        let tmp = body[headers_end..file_end].to_vec();
        let fist = tmp
            .windows(new_line_pattern.len())
            .position(|window| window == new_line_pattern)
            .unwrap_or_default();
        tmp[fist + 4..].to_vec()
    }
}
