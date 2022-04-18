# File Sharer

Easily share and recieve files, secured by an access token.

There are two web apps:

+ The "admin" app allows the server admin to generate new "shares" (admin provides users access to specific files) and "uploads" (admin allows users to upload files)
  + The admin app is only bound to localhost. Please use a reverse proxy if you wish to have wider access
  + The admin app can be disabled completely by passing the `--disable-admin-app` command line parameter
+ The "user" app allows users with the specific access token access to shares and uploads

## Usage

    File Sharer 
    Easily share and upload files, protected by access tokens

    USAGE:
        file-sharer.exe [OPTIONS]

    OPTIONS:
            --admin-port <ADMIN_PORT>
                The port to listen on for the admin app [default: 8000]

            --disable-admin-app
                Disable the admin app

            --files <FILES>
                Where to store files [default: .]

        -h, --help
                Print help information

        -p, --user-port <USER_PORT>
                The port to listen on for the user app. If not specified, uses port specified by --user-
                root

            --shares <SHARES>
                Where to store shares (relative to files) [default: shares]

            --silence-different-port-warning
                Silence the warning when --user-port differs from the port specified in --user-root

            --uploads <UPLOADS>
                Where to store uploads (relative to files) [default: uploads]

            --user-localhost-only
                Bind the user app to localhost only (useful for dev)

            --user-root <USER_ROOT>
                [default: http://localhost:8080]
