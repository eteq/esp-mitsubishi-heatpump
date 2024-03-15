"""Config flow for esp_mitsubishi_heatpump."""

import re

# import logging
#_LOGGER = logging.getLogger(__name__)

import voluptuous as vol

from homeassistant.core import callback
from homeassistant.components import zeroconf
from homeassistant.config_entries import ConfigFlow
from homeassistant.data_entry_flow import FlowResult
from homeassistant.const import CONF_HOST, CONF_NAME, CONF_PORT, CONF_MAC

from homeassistant.helpers.device_registry import format_mac

from .const import DOMAIN

DATA_SCHEMA = vol.Schema(
    {vol.Required(CONF_HOST): str, vol.Required(CONF_PORT, default=8923): int}
)

NAME_W_MAC_MATCH = re.compile(r'.*mac ([a-z0-9]*).*')


class ESPMitsubishiHeapumpFlowHandler(ConfigFlow, domain=DOMAIN):
    VERSION = 1

    def __init__(self) -> None:
        """Initialize flow."""
        self._host: str | None = None
        self._port: int | None = None
        self._device_name: str | None = None
        self._mac: str | None = None

    @callback
    def _async_get_entry(self):
        return self.async_create_entry(
            title=self._device_name,
            data={
                CONF_NAME: self._device_name,
                CONF_HOST: self._host,
                CONF_PORT: self._port,
                CONF_MAC: self._mac,
            },
        )

    async def async_step_user(self, user_input=None, error=None):
        """Handle the manual user addition step."""
        errors = {}
        if error is not None:
            errors["base"] = error

        if user_input is not None:
            self._host = user_input[CONF_HOST]
            self._port = user_input[CONF_PORT]
                
            raise NotImplementedError("need to get the device name from the host and port the user entered")
            if response is None:
                return await self.async_step_user(error="could not get device name")
            
            result = await self.get_mac_and_check_uid()
            if result is not True:
                return result
                
            return self._async_get_entry()

        return self.async_show_form(
            step_id="user",
            data_schema=DATA_SCHEMA,
            errors=errors,
            #description_placeholders={},
        )

    async def async_step_zeroconf(
        self, discovery_info: zeroconf.ZeroconfServiceInfo
        ) -> FlowResult:

        self._device_name = discovery_info.name
        self._host = discovery_info.host
        self._port = discovery_info.port

        result = await self.get_mac_and_check_uid()
        if result is not True:
            return result

        return await self.async_step_discovery_confirm()

    async def async_step_discovery_confirm(self, user_input=None):
        """Handle user-confirmation of discovered node."""
        if user_input is not None:
            return self._async_get_entry()

        return self.async_show_form(
            step_id="discovery_confirm", description_placeholders={"name": self._name}
        )

    
    async def get_mac_and_check_uid(self):
        macmatch = None
        if self._device_name is not None:
            macmatch = NAME_W_MAC_MATCH.match(self._device_name)
        if macmatch:
            self._mac = format_mac(macmatch.group(1))
        else:
            return self.async_abort(reason="mdns_name_missing_mac")


        # Check if already configured
        await self.async_set_unique_id(self._mac)
        self._abort_if_unique_id_configured(
            updates={CONF_HOST: self._host, CONF_PORT: self._port, CONF_NAME: self._device_name}
        )
        return True  # True means all good
