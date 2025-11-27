use mountain_mqtt::client::{ClientReceivedEvent, EventHandler, EventHandlerError};

use crate::{
    iot_topic,
    output,
    tcp,
};

use super::publish::mqtt_send;

pub(super) const MAX_APPLICATION_PROPERTIES: usize = 16;

pub(super) struct InboundEventHandler;

impl EventHandler<MAX_APPLICATION_PROPERTIES> for InboundEventHandler {
    async fn handle_event(
        &mut self,
        event: ClientReceivedEvent<'_, MAX_APPLICATION_PROPERTIES>,
    ) -> Result<(), EventHandlerError> {
        match event {
            ClientReceivedEvent::ApplicationMessage(message) => {
                match message.topic_name {
                    concat!(iot_topic!(), "/ctrl") => {
                        output::output_state_from_mqtt(message).await?;
                    }
                    concat!(iot_topic!(), "/rpc/tcp") => {
                        tcp::tcp_send(message.payload).await;
                    }
                    concat!(iot_topic!(), "/echo") => {
                        mqtt_send(message.payload, "/echo").await;
                    }
                    _ => {}
                }
                Ok(())
            }
            ClientReceivedEvent::Ack => Ok(()),
            ClientReceivedEvent::SubscriptionGrantedBelowMaximumQos {
                granted_qos,
                maximum_qos,
            } => {
                defmt::warn!(
                    "subscription granted at {} (requested {})",
                    granted_qos as u8,
                    maximum_qos as u8
                );
                Ok(())
            }
            ClientReceivedEvent::PublishedMessageHadNoMatchingSubscribers => {
                defmt::warn!("published message had no subscribers");
                Ok(())
            }
            ClientReceivedEvent::NoSubscriptionExisted => {
                defmt::warn!("unsubscribe ack reported no existing subscription");
                Ok(())
            }
        }
    }
}
