# Create API key for automated testing/Docker initialization
# Only executes if SETUP_ADMIN_EMAIL environment variable is provided

return unless ENV["SETUP_ADMIN_EMAIL"].present?

# Extract and validate environment variables
email = ENV["SETUP_ADMIN_EMAIL"].to_s.strip.downcase
password = ENV["SETUP_ADMIN_PASSWORD"].to_s
setup_key = ENV["SETUP_API_KEY"].to_s.strip
first_name = ENV["SETUP_FIRSTNAME"].to_s.strip
last_name = ENV["SETUP_LASTNAME"].to_s.strip
currency = ENV["SETUP_CURRENCY"].to_s.strip.upcase

# Apply defaults
setup_key = "auto" if setup_key.blank? || setup_key == "auto"
first_name = "Admin" if first_name.blank?
last_name = "User" if last_name.blank?
currency = "USD" if currency.blank?

# Validate password
unless password.present? && password.length >= 8
  puts "ERROR: SETUP_ADMIN_PASSWORD must be at least 8 characters"
  return
end

# Validate email format
unless email.match?(URI::MailTo::EMAIL_REGEXP)
  puts "ERROR: SETUP_ADMIN_EMAIL must be a valid email address"
  return
end

# Validate currency: must be a 3-letter ISO 4217 code recognised by the Money gem.
unless currency.match?(/\A[A-Z]{3}\z/) && Money::Currency.find(currency)
  puts "ERROR: SETUP_CURRENCY must be a valid 3-letter ISO 4217 currency code (e.g. USD, NZD)"
  return
end

# Generate or validate API key
if setup_key == "auto"
  api_key_value = ApiKey.generate_secure_key
else
  # Validate provided key format (64-character hex string)
  unless setup_key.match?(/\A[0-9a-f]{64}\z/)
    puts "ERROR: SETUP_API_KEY must be a 64-character hex string or 'auto'"
    return
  end
  api_key_value = setup_key
end

begin
  # Find or create user with family
  user = User.find_by(email: email)

  if user
    puts "Found existing user: #{email}"
  else
    # Create family first since user belongs_to :family is required
    family = Family.create!(
      name: "#{email.split('@').first.capitalize} Family",
      currency: currency
    )

    user = User.create!(
      email: email,
      password: password,
      password_confirmation: password,
      family: family,
      first_name: first_name,
      last_name: last_name,
      role: :admin,
      onboarded_at: Time.current
    )

    puts "Created new user: #{email}"
  end

  # Find or create API key for this user
  # Note: one_active_key_per_user_per_source validation ensures only one "web" key exists
  api_key = user.api_keys.find_by(source: "web")

  if api_key
    puts "Found existing API key for user (reusing)"
    puts "Email: #{user.email}"
    puts "API Key: #{api_key.plain_key}"
    puts "Scope: read_write"
  else
    # Create new API key
    api_key = user.api_keys.create!(
      name: "Setup API Key",
      key: api_key_value,
      scopes: [ "read_write" ],
      source: "web"
    )

    puts "Setup API Key created successfully"
    puts "Email: #{user.email}"
    puts "API Key: #{api_key.plain_key}"
    puts "Scope: read_write"
  end

rescue ActiveRecord::RecordInvalid => e
  puts "ERROR creating setup API key: #{e.message}"
  raise e
rescue StandardError => e
  puts "ERROR: #{e.message}"
  raise e
end
