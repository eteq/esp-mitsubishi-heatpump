# esp_mitsubishi_heatpump

This repo's goal is to provide a simple self-contained controller for Mitsubishi heat pumps that use the CN105 connector.  To that end it contains two things: a Rust-based firmware for esp32 microcontroller-based boards, and a home assistant integration to make a climate entity to match the contoller.

1. Compile the rust firmware and flash it onto your esp32cX's. Set ``WIFI_SSID`` AND ``WIFI_PASS`` environment variables to your local wifi network. You can also set ``TX_PIN``/``RX_PIN`` to set the pins to talk to the heatpump, although the default of 4/5 is known to work well.
2. Connect the esp32cX's to the CN105 connector
3. Repeat for all heat pumps you have
4. Install the home assistant integration (see hass_integration/README.md for more.)
5. Profit!

Alternatively/additionally, you can do just 1-3 and directly connect to the controllers on your local network to control the heat pumps. ``http://heatpump-controller-{MAC ADDRESS}.local:8923/index.html`` should do the job.

## Hardware

For more on the details of the CN105 connector, see https://chrdavis.github.io/hacking-a-mitsubishi-heat-pump-Part-1/ . Note that for me it worked to just connect the 5V on CN105  directly to the esp32cX as well as the TX/RX lines without any level shifters.  This is probably hardware-dependent though.

In principle any esp32 IDF-compatible board should work, but this has only been tested thus for on esp32c6 boards. It has also only been tested with MSZFH##NA heat pumps, although other related projects (see below) seem to indicate the protocol is the same for a wide range of other Mitsubishi mini-split heat pumps.

## Acknowledgements

This would have been impossible without the work in https://github.com/SwiCago/HeatPump and https://github.com/m000c400/Mitsubishi-CN105-Protocol-Decode, which provided enough info about Mitsubishi's UART protocol to make this repo possible.

