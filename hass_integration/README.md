# home assistant custom integration for esp_mitsubishi_heatpump

This is a home assistant integration that goes hand in hand with the rust-based esp32cX firmware.  The following should do the job to install it:

1. Copy everything in this directory to ``YOUR_HOME_ASSISTANT_CONFIG_DIR/custom_components/esp_mitsubishi_heatpump`` (creating any missing directories).
2. Add a line with just``esp_mitsubishi_heatpump:`` to your home assistant instance's ``configuration.yaml``
3. (Re)start home assistant

You should now see entities for all attached heat pump controllers.

TODO: switch to a config flow and integrate with HACS;
