use crate::{Config, ServerError};

use super::Request;
pub use super::{Server, Session};
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token};
use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::net::ToSocketAddrs;

// -------------------------------------------------------------------------------------
// ROUTER
// -------------------------------------------------------------------------------------
const CLIENT_START: Token = Token(1000); // Token de départ pour les clients

#[derive(Debug)]
pub struct Router {
    pub servers: Vec<Server>,
    pub sessions: HashMap<Token, Session>,
    pub listeners: HashMap<Token, TcpListener>, // Associe un token à un TcpListener
    pub clients: HashMap<Token, TcpStream>,     // Associe un token à un TcpStream
    pub next_token: usize,
    pub request_queue: Vec<Request>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            servers: vec![],
            sessions: HashMap::new(),
            listeners: HashMap::new(),
            clients: HashMap::new(),
            next_token: CLIENT_START.0,
            request_queue: vec![],
        }
    }

    /// Ajoute un serveur et démarre l'écoute sur ses ports.
    pub fn add_server(&mut self, server: Server) -> io::Result<()> {
        for &port in &server.ports {
            let addr = format!("{}:{}", server.ip_addr, port)
                .to_socket_addrs()?
                .next()
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Other, "Impossible de résoudre l'adresse")
                })?;
            println!("Adresse de connexion {}", addr);
            let listener = TcpListener::bind(addr)?;
            let token = Token(self.next_token - 1000);
            self.next_token += 1;
            self.listeners.insert(token, listener);
        }
        self.servers.push(server);
        Ok(())
    }

    pub fn remove_server(&mut self, server: Server) -> io::Result<()> {
        // Filtrer les serveurs pour supprimer celui qui correspond
        self.servers
            .retain(|s| s.ip_addr != server.ip_addr && s.hostname != server.hostname);

        // Fermer les listeners associés à ce serveur
        for &port in &server.ports {
            // Trouver le token associé à ce port
            let token = self
                .listeners
                .iter()
                .find(|(_, listener)| {
                    listener.local_addr().ok().map(|addr| addr.port()) == Some(port)
                })
                .map(|(token, _)| *token);
            if let Some(token) = token {
                // Fermer le listener en le retirant de la HashMap
                self.listeners.remove(&token);
                println!("Listener fermé pour le port {}", port);
            }
        }

        Ok(())
    }
    /// Démarre le Router et commence à écouter les événements.
    pub fn run(&mut self, config: &Config) -> io::Result<()> {
        let mut poll = Poll::new()?;
        let mut server_tokens = HashMap::new();

        // Enregistrer chaque listener avec un token unique
        for (token, listener) in &mut self.listeners {
            poll.registry()
                .register(listener, *token, Interest::READABLE | Interest::WRITABLE)?;
            server_tokens.insert(*token, listener.local_addr()?);
        }

        let mut events = Events::with_capacity(config.log_files.events_limit);

        loop {
            poll.poll(&mut events, None)?;

            for event in events.iter() {

                if event.is_error() || event.is_read_closed() {
                    // Nettoyer les tokens inactifs
                    if let Some(mut stream) = self.clients.remove(&event.token()) {
                        poll.registry().deregister(&mut stream)?;
                        let addr = stream.peer_addr()?;
                        let mut err_req = Request::default();
                        err_req.host = addr.to_string();

                        Server::error_log(
                            &err_req,
                            config,
                            "access_log",
                            file!(),
                            line!(),
                            ServerError::IOError(&io::Error::new(
                                ErrorKind::BrokenPipe,
                                format!("Stream fermé pour le client {}", addr),
                            )),
                        );
                        stream.shutdown(std::net::Shutdown::Both)?;
                    };
                    continue;
                }
                if let Some(_) = server_tokens.get(&event.token()) {
                    // Nouvelle connexion sur un TcpListener
                    self.accept_connection(event.token(), &poll)?;
                    // println!("Nouvelle connexion sur le port {}", addr.port());
                } else {
                    // Données reçues sur un TcpStream
                    let stream = (self.clients.get_mut(&event.token()))
                        .expect("Erreur lors de la recupération du canal tcpstream");
                    let req = Request::read_request(stream);
                    let mut cookie = req.id_session.clone();
                    // println!("cookie extract: {}",cookie);
                    let client_token = Token(self.next_token);
                    self.next_token += 1;
                    // Tentative de récupération du cookie
                    if !cookie.is_empty() {
                        // Recherche d'une session existante avec le même cookie
                        let mut session_found = false;

                        for (old_token, session) in self.sessions.clone().iter() {
                            if session.id.trim() == cookie && !session.is_expired() {
                                let mut new_session = Session::new();
                                new_session.id = session.id.clone();
                                self.sessions.remove(&old_token);
                                self.sessions.insert(client_token.clone(), new_session);
                                session_found = true;
                                break;
                            }
                        }

                        if !session_found {
                            // Si aucune session existante n'est trouvée, créez une nouvelle session
                            let new_session = Session::new();
                            self.sessions
                                .insert(client_token.clone(), new_session.clone());
                        }
                    } else {
                        // Si aucun cookie n'est trouvé, créez une nouvelle session
                        let new_session = Session::new();
                        self.sessions
                            .insert(client_token.clone(), new_session.clone());
                    }

                    if let Some(session) = self.sessions.get_mut(&client_token) {
                        cookie = Session::make_cookie(
                            "cookie_01",
                            &*session.id,
                            session.expiration_time,
                        );
                    }

                    if req.method == "GET" || req.method == "POST" {
                        self.request_queue.push(req);
                    } else {
                        for (i, waiting_req) in self.request_queue.clone().iter().enumerate() {
                            if waiting_req.method == "POST" {
                                if let Some(content_length) = waiting_req.content_length {
                                    println!("content-length: {} <======> body len: {}", content_length, req.body.len());
                                    if content_length > waiting_req.body.len()  {
                                        if let Some(boundary) = waiting_req.boundary.clone() {
                                            if req.body.contains(&boundary) {
                                                self.request_queue[i].body.push_str(&req.body);
                                                self.request_queue[i]
                                                    .body_byte
                                                    .extend_from_slice(&req.body_byte);
                                                if let Some(content_length) =
                                                    self.request_queue[i].content_length
                                                {
                                                    if self.request_queue[i].body.len()
                                                        >= content_length
                                                    {
                                                        self.request_queue[i].complete = true;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Self::route_request(
                        &mut self.request_queue,
                        self.servers.clone(),
                        stream,
                        cookie,
                        &config,
                    );
                }
            }
        }
    }

    /// Accepte une nouvelle connexion et l'ajoute à la liste des clients.
    fn accept_connection(&mut self, token: Token, poll: &Poll) -> io::Result<()> {
        if let Some(listener) = self.listeners.get_mut(&token) {
            let (mut stream, _) = listener.accept()?;
            let client_token = Token(self.next_token);
            self.next_token += 1;
            poll.registry()
                .register(&mut stream, client_token, Interest::READABLE)?;
            self.clients.insert(client_token, stream);
        }
        Ok(())
    }

    // Route une requête HTTP et génère une réponse.
    pub fn route_request(
        request_queue: &mut Vec<Request>,
        servers: Vec<Server>,
        stream: &mut TcpStream,
        cookie: String,
        config: &Config,
    ) {
        // On récupère le hostname, l'adresse ip et le port de la requête
        // On parcoure la liste des serveurs et on vérifie lequel a le hostname, le port et l'ip correspondant
        for (i, req) in request_queue.clone().into_iter().enumerate() {
            for server in servers.iter() {
                if server.ip_addr == req.host && server.ports.contains(&req.port) {
                    if req.method == "GET" {
                        server.handle_request(stream, req.clone(), cookie.clone(), config);
                        request_queue.remove(i);
                        break;
                    } else if req.complete {
                        println!("arret possible");
                        server.handle_request(stream, req.clone(), cookie.clone(), config);
                        request_queue.remove(i);
                        break;
                    }
                }
            }
        }
    }
}
