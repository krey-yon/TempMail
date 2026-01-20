use crate::types::{CurrentStates, SMTPResult};


const MAX_EMAIL_SIZE: usize = 10_485_760;


pub struct HandleCurrentState {
    current_state: CurrentStates,
    greeting_message: String,
    max_email_size: usize,
}


impl HandleCurrentState {
    pub fn new(server_domain: impl AsRef<str>) -> Self {
        let server_domain = server_domain.as_ref();
        let greeting_message = format!(
            "250-{server_domain} greets {server_domain}\n\
             250-SIZE {}\n\
             250 8BITMIME\n",
            MAX_EMAIL_SIZE
        );

        Self {
            current_state: CurrentStates::Initial,
            greeting_message,
            max_email_size: MAX_EMAIL_SIZE,
        }
    }

    pub async fn process_smtp_command<'a> (
        &mut self,
        client_message: &str,
    ) -> SMTPResult<'a, &[u8]>  {
        todo!()
    }
}
