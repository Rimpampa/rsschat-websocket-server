// #![windows_subsystem = "windows"]

use log::{debug, error, info, warn};
use websocket::server::upgrade::{sync::Buffer, WsUpgrade};
use websocket::sync::Server;
use websocket::sync::{Reader, Writer};
use websocket::Message;
use websocket::OwnedMessage;

use std::fmt;
use std::net::TcpStream;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
use std::thread;

enum IncomingMessageType {
    Text(String),
    Invalid,
    Disconnected,
}

#[derive(Clone)]
enum OutgoingMessageType {
    Valid(Arc<String>, Arc<String>),
    Disconnected(Arc<String>),
    Connected(Arc<String>),
    Invalid,
    EndThread,
}

impl fmt::Debug for OutgoingMessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Format the message for sending it to the client
        match self {
            OutgoingMessageType::Valid(name, text) => write!(f, "FROM {}\n{}", name, text),
            OutgoingMessageType::Disconnected(name) => write!(f, "LEFT {}", name),
            OutgoingMessageType::Connected(name) => write!(f, "JOIN {}", name),
            OutgoingMessageType::Invalid => write!(f, "INVL"),
            // This message is for the thread not the client,
            // thus it won't ever be formatted
            OutgoingMessageType::EndThread => unreachable!(),
        }
    }
}

type IncomingMessage = (usize, IncomingMessageType);
type OutgoingMessage = OutgoingMessageType;

enum ClientState {
    Connected(Option<Arc<String>>, mpsc::Sender<OutgoingMessage>),
    // Pending(spsc::Producer<OutgoingMessage>),
    Disconnected,
}

impl ClientState {
    pub fn is_connected(&self) -> bool {
        match self {
            ClientState::Connected(Some(_), _) => true,
            _ => false,
        }
    }
    // pub fn is_pending(&self) -> bool {
    //     match self {
    //         ClientState::Connected(None, _) => true,
    //         _ => false,
    //     }
    // }
    pub fn is_disconnected(&self) -> bool {
        match self {
            ClientState::Disconnected => true,
            _ => false,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            ClientState::Connected(Some(ref name), _) => name,
            ClientState::Connected(None, _) => "[pending]",
            ClientState::Disconnected => "[disconnected]",
        }
    }
}

fn main() {
    // Initialize the logging API
    log4rs::init_file("log/config.yml", Default::default()).unwrap();

    // Bind the server to the port number 5547
    let server: Server<_> = Server::bind("0.0.0.0:5547").unwrap();
    info!("SERVICE STARTED");

    // Create the channels for comunicating with the clients manager
    let (manager_tx, manager_rx) = mpsc::channel();

    // Spawn the clients manager thread
    thread::spawn(move || clients_manager(manager_rx));

    // For every request
    for request in server.filter_map(Result::ok) {
        debug!("REQUEST {}", request.uri());

        // Move the comunication on another thread
        let mngtx = manager_tx.clone();
        thread::spawn(move || client_request_handler(request, mngtx));
    }
}

fn clients_manager(
    incoming_clients: mpsc::Receiver<(mpsc::Sender<OutgoingMessage>, Reader<TcpStream>)>,
    // incoming_messages: mpsc::Receiver<IncomingMessage>,
    // message_sender: mpsc::Sender<IncomingMessage>,
) {
    // Create the channels for sending the incoming messages to the manager thread
    let (message_sender, incoming_messages) = mpsc::channel::<IncomingMessage>();

    // List of the sender channels of the connected clients
    let mut clients: Vec<ClientState> = Vec::new();
    // Indicator of the number of empty positions inside the list
    let mut empty = 0;
    // The list is handled so that when a user disconnects the empty variable
    // is incremented and when an user joins (if there is an empty spot) it
    // will occupy that position

    loop {
        // Check if a client is trying to connect
        match incoming_clients.try_recv() {
            // If the sending channel disconnects (drop) then there is an error
            Err(TryRecvError::Disconnected) => {
                error!("SERVER LISTENER STOPPED WORKING");
                panic!();
            }
            // No client is trying to connect
            Err(TryRecvError::Empty) => (),
            // A client is trying to connect
            Ok((client, message_reader)) => {
                // Generate an id for the new client:
                let id;
                // If there is at least an empty position
                if empty > 0 {
                    // Search the closest empty position
                    match clients
                        .iter_mut()
                        .enumerate()
                        .find(|(_, c)| c.is_disconnected())
                    {
                        Some((x, p)) => {
                            // Put the producer there
                            *p = ClientState::Connected(None, client);
                            // An empty position was filled
                            empty -= 1;
                            // Set the id
                            id = x;
                        }
                        None => unreachable!(),
                    }
                }
                // If there isn't an empty position, create a new one
                else {
                    // Set the id
                    id = clients.len();
                    // Push the producer in the new position
                    clients.push(ClientState::Connected(None, client));
                }
                // Spawn the reciever thread
                let msgtx = message_sender.clone();
                thread::spawn(move || client_receiver(id, msgtx, message_reader));
            }
        }
        // Check if there are any incoming messages
        match incoming_messages.try_recv() {
            // If the sending channel disconnects (drop) then there is an error
            Err(TryRecvError::Disconnected) => {
                error!("MESSAGES LISTENER STOPPED WORKING");
                panic!();
            }
            // No message has arrived
            Err(TryRecvError::Empty) => (),
            // A message has arrived
            Ok((id, IncomingMessageType::Text(message))) => {
                info!(
                    "TEXT MESSAGE FROM CLIENT #{} ({:?})",
                    id,
                    clients[id].name()
                );

                // Check the type of client that sent the message
                match &mut clients[id] {
                    // If it's a connected client send the message to the other connected clients
                    ClientState::Connected(Some(ref name), _) => {
                        let arc_name = Arc::clone(name);
                        // brodcast_message(&clients, id, arc_name, message);

                        // Create an arc with the message inside, this allows to clone the
                        // message on the sending thread so that it doesn't slow down this one
                        // let arc_msg = Arc::new((name, message));
                        let arc_msg = OutgoingMessageType::Valid(arc_name, Arc::new(message));
                        brodcast_message(&mut clients, &mut empty, id, arc_msg);
                    }
                    // If it's a pending client the message will be its name
                    ClientState::Connected(name, _) => {
                        // Check if it's a valid name
                        let arc_name = Arc::new(message);
                        *name = Some(Arc::clone(&arc_name));
                        // brodcast_message(&clients, id, Arc::clone(arc_name), "Connected!".into());

                        // Create an arc with the message inside, this allows to clone the
                        // message on the sending thread so that it doesn't slow down this one
                        // let arc_msg = Arc::new((name, message));
                        let arc_msg = OutgoingMessageType::Connected(arc_name);
                        brodcast_message(&mut clients, &mut empty, id, arc_msg);
                    }
                    // Disconnected clients can't send messages
                    ClientState::Disconnected => unreachable!(),
                }
            }
            // The client has disconnected
            Ok((id, IncomingMessageType::Disconnected)) => {
                info!("CLIENT #{} HAS DISCONNECTED ({:?})", id, clients[id].name());

                match &clients[id] {
                    // A connected client has disconnected
                    ClientState::Connected(Some(name), tx) => {
                        // brodcast_message(&clients, id, Arc::clone(name), "Disconnected!".into());

                        // End the client message sender thread
                        tx.send(OutgoingMessageType::EndThread).ok();

                        // Create an arc with the message inside, this allows to clone the
                        // message on the sending thread so that it doesn't slow down this one
                        // let arc_msg = Arc::new((name, message));
                        let arc_msg = OutgoingMessageType::Disconnected(Arc::clone(name));
                        brodcast_message(&mut clients, &mut empty, id, arc_msg);
                    }
                    // A pending client has disconnected, end its message sender thread
                    ClientState::Connected(None, tx) => {
                        tx.send(OutgoingMessageType::EndThread).unwrap_or(())
                    }
                    // Disconnected clients can't send messages
                    ClientState::Disconnected => unreachable!(),
                }
                // Remove the client from the list and count the empty position
                clients[id] = ClientState::Disconnected;
                empty += 1;
            }
            // The client send a non-text message
            Ok((id, IncomingMessageType::Invalid)) => {
                info!(
                    "INVALID MESSAGE FROM CLIENT #{} ({:?})",
                    id,
                    clients[id].name()
                );

                match &clients[id] {
                    ClientState::Connected(_, tx) => {
                        // Tell the client that its message was invalid
                        if tx.send(OutgoingMessageType::Invalid).is_err() {
                            warn!("CLIENT #{} MESSAGE SENDER THREAD HAS STOPPED WORKING", id);
                            clients[id] = ClientState::Disconnected;
                            empty += 1;
                        }
                    }
                    // Disconnected clients can't send messages
                    ClientState::Disconnected => unreachable!(),
                }
            }
        }
    }
}

fn brodcast_message(
    clients: &mut Vec<ClientState>,
    empty_slots: &mut usize,
    from: usize,
    message: OutgoingMessage,
) {
    // Thake the clients before this id
    clients
        .iter_mut()
        .enumerate()
        // Remove the clients that are disconnected or pending
        .filter(|(id, client)| client.is_connected() && *id != from)
        // Send the message to the clients
        .for_each(|(id, client)| {
            if match client {
                ClientState::Connected(Some(_), tx) => tx.send(message.clone()),
                _ => unreachable!(),
                // Check for errors
            }
            .is_err()
            {
                *client = ClientState::Disconnected;
                warn!("CLIENT #{} MESSAGE SENDER THREAD HAS STOPPED WORKING", id);
                *empty_slots += 1;
            }
        });
}

fn client_request_handler(
    request: WsUpgrade<TcpStream, Option<Buffer>>,
    manager: mpsc::Sender<(mpsc::Sender<OutgoingMessage>, Reader<TcpStream>)>,
) {
    // Get the address of the incoming client
    let client_addr = request.tcp_stream().peer_addr().unwrap();

    // Reject the request if the clients doesn't support the connect4 sub-protocol
    if !request.protocols().contains(&"rsschat".into()) || request.uri() != "/chat" {
        request.reject().ok();
        info!("CLIENT REJECTED <{}>", client_addr);
    }
    // Otherwise accept the client request
    else if let Ok(client) = request.use_protocol("rsschat").accept() {
        info!("CLIENT CONNECTED <{}>", client_addr);
        if let Ok((tcp_rx, tcp_tx)) = client.split() {
            // Create the channels for comunicating with the main thread
            let (messages_tx, messages_rx) = mpsc::channel();

            // Give the manager thread the channel for sending massages to this client
            // and the channel that the client will use to send the messages to the
            // manager thread
            if manager.send((messages_tx, tcp_rx)).is_err() {
                error!("SERVER LISTENER STOPPED WORKING");
                panic!();
            }
            // Spawn the sender thread
            thread::spawn(move || client_sender(messages_rx, tcp_tx));
        }
        // The stream cannot be splitted
        else {
            warn!("CANNOT SPLIT THE STREAM OF <{}>", client_addr);
        }
    }
    // The connection upgrade failed
    else {
        warn!("CONNECTION UPGRADE FAILED <{}>", client_addr);
    }
}

fn client_receiver(id: usize, tx: mpsc::Sender<IncomingMessage>, mut rx: Reader<TcpStream>) {
    // Wait until a valid name is received
    let mut valid = false;
    while !valid {
        // Wait for the name of the client to arrive and check if it's valid
        if match rx.recv_message() {
            // It's a correct name
            Ok(OwnedMessage::Text(message)) if check_name(&message) => {
                valid = true;
                tx.send((id, IncomingMessageType::Text(message)))
            }
            // It's a non-text message, inform the manager thread
            Ok(_) => tx.send((id, IncomingMessageType::Invalid)),
            // There was an error reciving this message, so the client has disconnected
            Err(_) => {
                // Ignore any error and end the thread execution
                tx.send((id, IncomingMessageType::Disconnected)).ok();
                return;
            }
        }
        // If there is an error while sending the message
        // the manager thread is not working correctly
        .is_err()
        {
            warn!(
                "LOST COMUNICATION WITH THE MANAGER THREAD (CLIENT RECEIVER #{})",
                id
            );
            return;
        }
    }
    loop {
        // Wait for a message to arrive and check it's type
        if match rx.recv_message() {
            // It's a text message, send it to the manager thread
            Ok(OwnedMessage::Text(message)) => tx.send((id, IncomingMessageType::Text(message))),
            // It's a non-text message, inform the manager thread
            Ok(_) => tx.send((id, IncomingMessageType::Invalid)),
            // There was an error reciving this message, so the client has disconnected
            Err(_) => {
                // Ignore any error and end the thread execution
                tx.send((id, IncomingMessageType::Disconnected)).ok();
                return;
            }
        }
        // If there is an error while sending the message
        // the manager thread is not working correctly
        .is_err()
        {
            warn!(
                "LOST COMUNICATION WITH THE MANAGER THREAD (CLIENT RECEIVER #{})",
                id
            );
            return;
        }

        // // Wait for a message to arrive and check it's type
        // if tx.send((id, match rx.recv_message() {
        //     // It's a text message, send it to the manager thread
        //     Ok(OwnedMessage::Text(message)) => IncomingMessageType::Text(message),
        //     // It's a non-text message, inform the manager thread
        //     Ok(_) => IncomingMessageType::Invalid,
        //     // There was an error reciving this message, so the client has disconnected
        //     Err(_) => IncomingMessageType::Disconnected,

        // // If there is an error while sending the message
        // // the manager thread is not working correctly
        // })).is_err() {
        //     error!("MESSAGES LISTENER STOPPED WORKING");
        //     panic!();
        // }
    }
}

fn client_sender(rx: mpsc::Receiver<OutgoingMessage>, mut tx: Writer<TcpStream>) {
    loop {
        // Wait for a message to arrive (from another client)
        match rx.recv() {
            // The client has left the chat, close this thread
            Ok(OutgoingMessageType::EndThread) => return,
            Ok(message) => tx
                .send_message(&Message::text(format!("{:?}", message)))
                .ok(),
            Err(_) => {
                warn!("LOST THE COMUNICATION WITH THE MANAGER THREAD");
                return;
            }
        };
    }
}

fn check_name(name: &str) -> bool {
    if name.len() > 32 {
        return false;
    }
    for ch in name.chars() {
        if ch.is_control() {
            return false;
        }
    }
    true
}
