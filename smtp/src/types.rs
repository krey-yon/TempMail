use crate::errors::SmtpResponseError;

pub struct Email {
    pub sender: String,
    pub recipients: Vec<String>,
    pub content: String,
    pub size: usize,
}

pub enum CurrentStates {
    Initial,
    Greeted,
    AwaitingRecipient(Email),
    AwaitingData(Email),
    DataRecieved(Email),
}


pub type SMTPResult<'a, T> = Result<T, SmtpResponseError<'a>>;