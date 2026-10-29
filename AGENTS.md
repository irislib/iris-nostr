Keep this repository separate from Iris Chat. The executable is iris-nostr;
iris is already owned by Iris Chat. Messaging, signing keys and ratchet state
remain owned by Iris Chat. Use shared nostr-pubsub, Hashtree and graph libraries.
Do not open another application's live database or invent a second FIPS pool
when its explicit daemon interface can be used. Offline must perform no network
I/O, including through a local daemon. Keep library APIs free of CLI types.
Use locked builds and test with synthetic identities and local fixtures.
